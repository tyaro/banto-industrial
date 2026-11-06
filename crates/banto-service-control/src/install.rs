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
//! ただし削除保留・その判定不能は成功にしない。既設/作成済みの両方を
//! DeleteFlagで設定処理の前後に確かめる（OS内部値依存、最終検査後の競合は残る）。
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

/// [`install_service`] の失敗。新規作成後の検査失敗では登録が残ることがある。
/// 自動 rollback はしない。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallError {
    /// SCM に接続できなかった（多くは権限不足）。
    Connect(String),
    /// サービスの削除が保留中。成功扱いせず、他の管理ツールを閉じて再実行する。
    DeletePending,
    /// 削除保留かどうかを確かめられなかった。
    CheckDeletion(String),
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
    /// サービスが登録されていない（`ERROR_SERVICE_DOES_NOT_EXIST` = 1060。文字列ではなく
    /// OS の番号で見分ける）。
    NotInstalled,
    /// サービスを開けなかった（権限不足など。未登録は [`UninstallError::NotInstalled`]）。
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

    match manager.open_service(spec.name, ServiceAccess::QUERY_CONFIG) {
        // handle は前後の検査と設定処理の完了まで保有する。
        Ok(_existing) => finish_install(
            InstallKind::AlreadyRegistered,
            || deletion_registry::read(spec.name),
            || {
                Ok((
                    None,
                    preshutdown_report(&ensure_preshutdown_timeout(
                        spec.name,
                        spec.preshutdown_timeout,
                    )),
                ))
            },
        ),
        Err(err) => {
            if is_delete_pending_error(&err) {
                return Err(InstallError::DeletePending);
            }
            // 既存の方針: それ以外の open 失敗は作成を試み、作成失敗を返す。
            let service = manager
                .create_service(&service_info(spec), ServiceAccess::CHANGE_CONFIG)
                .map_err(|err| {
                    if is_delete_pending_error(&err) {
                        InstallError::DeletePending
                    } else {
                        InstallError::Create(err.to_string())
                    }
                })?;
            finish_install(
                InstallKind::Created,
                || deletion_registry::read(spec.name),
                || {
                    let description_error = match spec
                        .description
                        .and_then(|description| service.set_description(description).err())
                    {
                        Some(err) if is_delete_pending_error(&err) => {
                            return Err(InstallError::DeletePending)
                        }
                        other => other.map(|err| err.to_string()),
                    };
                    Ok((
                        description_error,
                        preshutdown_report(&ensure_preshutdown_timeout(
                            spec.name,
                            spec.preshutdown_timeout,
                        )),
                    ))
                },
            )
        }
    }
}

/// OS 内部の DeleteFlag に依存する前後検査。最終検査後の外部削除との競合は防がない。
#[cfg_attr(not(windows), allow(dead_code))]
fn finish_install(
    kind: InstallKind,
    mut check: impl FnMut() -> Result<bool, String>,
    configure: impl FnOnce() -> Result<(Option<String>, PreshutdownReport), InstallError>,
) -> Result<InstallOutcome, InstallError> {
    let mut require_not_pending = || match check() {
        Ok(false) => Ok(()),
        Ok(true) => Err(InstallError::DeletePending),
        Err(err) => Err(InstallError::CheckDeletion(err)),
    };
    require_not_pending()?;
    let (description_error, preshutdown) = configure()?;
    require_not_pending()?;
    Ok(InstallOutcome {
        kind,
        description_error,
        preshutdown,
    })
}

#[cfg(windows)]
fn is_delete_pending_error(err: &windows_service::Error) -> bool {
    matches!(err, windows_service::Error::Winapi(io) if io.raw_os_error() == Some(1072))
}

#[cfg(windows)]
mod deletion_registry {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS};
    use windows_sys::Win32::System::Registry::{
        RegCloseKey, RegOpenKeyExW, RegQueryValueExW, HKEY, HKEY_LOCAL_MACHINE, KEY_QUERY_VALUE,
        REG_DWORD,
    };

    fn wide(s: &str) -> Vec<u16> {
        std::ffi::OsStr::new(s).encode_wide().chain([0]).collect()
    }
    struct Key(HKEY);
    impl Drop for Key {
        fn drop(&mut self) {
            // SAFETY: RegOpenKeyExWで開いたまだ閉じていないキー。
            unsafe {
                RegCloseKey(self.0);
            }
        }
    }
    pub(super) fn read(service: &str) -> Result<bool, String> {
        read_in(
            HKEY_LOCAL_MACHINE,
            r"SYSTEM\CurrentControlSet\Services",
            service,
        )
    }
    pub(super) fn read_in(root: HKEY, base: &str, service: &str) -> Result<bool, String> {
        if service.is_empty() || service.contains(['\\', '/', '\0']) {
            return Err("サービス名が不正です".into());
        }
        let path = wide(&format!(r"{base}\{service}"));
        let mut key = std::ptr::null_mut();
        // SAFETY: NUL 終端入力と有効な出力。開くのは読取り権限だけ。
        let status = unsafe { RegOpenKeyExW(root, path.as_ptr(), 0, KEY_QUERY_VALUE, &mut key) };
        if status != ERROR_SUCCESS {
            return Err(format!(
                "{service}: サービスキーを読めません: {}",
                std::io::Error::from_raw_os_error(status as i32)
            ));
        }
        let key = Key(key);
        let mut kind = 0;
        let mut value = 0u32;
        let mut bytes = std::mem::size_of::<u32>() as u32;
        // SAFETY: 値の出力先は bytes の大きさを持つ。型・サイズは使用前に検査する。
        let status = unsafe {
            RegQueryValueExW(
                key.0,
                wide("DeleteFlag").as_ptr(),
                std::ptr::null(),
                &mut kind,
                &mut value as *mut u32 as *mut u8,
                &mut bytes,
            )
        };
        classify_value(status, kind, bytes, value).map_err(|err| format!("{service}: {err}"))
    }
    pub(super) fn classify_value(
        status: u32,
        kind: u32,
        bytes: u32,
        value: u32,
    ) -> Result<bool, String> {
        if status == ERROR_FILE_NOT_FOUND {
            return Ok(false);
        }
        if status != ERROR_SUCCESS {
            return Err(format!(
                "DeleteFlag を読めません: {}",
                std::io::Error::from_raw_os_error(status as i32)
            ));
        }
        if kind != REG_DWORD || bytes != 4 {
            return Err(format!(
                "DeleteFlag の型またはサイズが不正です: type={kind}, bytes={bytes}"
            ));
        }
        match value {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(format!("DeleteFlag が未知の値です: {value}")),
        }
    }
}

/// `ERROR_SERVICE_DOES_NOT_EXIST`。
pub const ERROR_SERVICE_DOES_NOT_EXIST: i32 = 1060;

/// Win32 の番号（`raw_os_error()`）から `open_service` の失敗を分ける（純関数）。未登録
/// （1060）は [`UninstallError::NotInstalled`]、それ以外は OS の理由（`message`）つきの
/// [`UninstallError::Open`]。
pub fn classify_open_failure(os_error: Option<i32>, message: String) -> UninstallError {
    if os_error == Some(ERROR_SERVICE_DOES_NOT_EXIST) {
        UninstallError::NotInstalled
    } else {
        UninstallError::Open(message)
    }
}

/// `open_service` の失敗を分ける。`windows_service::Error` の表示は "IO error in winapi
/// call" だけなので、Win32 の失敗は OS の理由と番号を文言に含め、番号で未登録を見分ける
/// （#628）。
#[cfg(windows)]
fn classify_open_error(err: &windows_service::Error) -> UninstallError {
    match err {
        windows_service::Error::Winapi(io) => {
            classify_open_failure(io.raw_os_error(), io.to_string())
        }
        other => UninstallError::Open(other.to_string()),
    }
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
        .map_err(|err| classify_open_error(&err))?;

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

    let manager = WinScm::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)
        .map_err(|err| crate::ServiceManagerError::Other(err.to_string()))?;
    let service = match manager.open_service(name, ServiceAccess::QUERY_CONFIG) {
        Ok(service) => service,
        Err(windows_service::Error::Winapi(io))
            if io.raw_os_error() == Some(ERROR_SERVICE_DOES_NOT_EXIST) =>
        {
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

    #[test]
    fn completion_checks_before_and_after_configuration_without_suppressing_warnings() {
        use std::cell::Cell;
        for kind in [InstallKind::Created, InstallKind::AlreadyRegistered] {
            for (checks, configured, expected) in [
                (vec![Ok(false), Ok(false)], true, Ok(())),
                (vec![Ok(true)], false, Err(InstallError::DeletePending)),
                (
                    vec![Ok(false), Ok(true)],
                    true,
                    Err(InstallError::DeletePending),
                ),
                (
                    vec![Err("before".into())],
                    false,
                    Err(InstallError::CheckDeletion("before".into())),
                ),
                (
                    vec![Ok(false), Err("after".into())],
                    true,
                    Err(InstallError::CheckDeletion("after".into())),
                ),
            ] {
                let called = Cell::new(false);
                let mut checks = checks.into_iter();
                let result = finish_install(
                    kind,
                    || checks.next().expect("unexpected check"),
                    || {
                        called.set(true);
                        Ok((
                            Some("description warning".into()),
                            PreshutdownReport::Warning("ordinary configuration warning".into()),
                        ))
                    },
                );
                assert_eq!(called.get(), configured);
                assert!(checks.next().is_none());
                assert_eq!(result.as_ref().map(|_| ()).map_err(Clone::clone), expected);
                if let Ok(outcome) = result {
                    assert_eq!(outcome.kind, kind);
                    assert!(matches!(outcome.preshutdown, PreshutdownReport::Warning(_)));
                    assert_eq!(
                        outcome.description_error.as_deref(),
                        Some("description warning")
                    );
                }
            }
        }
        let result = finish_install(
            InstallKind::Created,
            || Ok(false),
            || Err(InstallError::DeletePending),
        );
        assert_eq!(result, Err(InstallError::DeletePending));
    }

    #[cfg(windows)]
    #[test]
    fn pending_is_classified_by_raw_os_number() {
        for code in [5, 1060, 1072, 1073] {
            let err = windows_service::Error::Winapi(std::io::Error::from_raw_os_error(code));
            assert_eq!(is_delete_pending_error(&err), code == 1072);
        }
    }

    #[cfg(windows)]
    #[test]
    fn deletion_registry_uses_only_its_temporary_hkcu_key() {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::System::Registry::*;
        fn wide(s: &str) -> Vec<u16> {
            std::ffi::OsStr::new(s).encode_wide().chain([0]).collect()
        }
        struct TestKey(HKEY);
        impl Drop for TestKey {
            fn drop(&mut self) {
                // SAFETY: このfixtureが開いたキーを一度だけ閉じる。
                unsafe {
                    RegCloseKey(self.0);
                }
            }
        }
        struct Fixture(String);
        impl Drop for Fixture {
            fn drop(&mut self) {
                // SAFETY: uniqueなテスト専用HKCU subtreeだけを除去する。
                unsafe {
                    RegDeleteTreeW(HKEY_CURRENT_USER, wide(&self.0).as_ptr());
                }
            }
        }
        let base = format!(
            r"Software\banto-service-control-test-delete-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let fixture = Fixture(base);
        let mut key = std::ptr::null_mut();
        let path = wide(&format!(r"{}\Test", fixture.0));
        // SAFETY: HKCUのunique専用パス、NUL終端入力と有効な出力。
        assert_eq!(
            unsafe { RegCreateKeyW(HKEY_CURRENT_USER, path.as_ptr(), &mut key) },
            0
        );
        let key = TestKey(key);
        let name = wide("DeleteFlag");
        assert_eq!(
            deletion_registry::read_in(HKEY_CURRENT_USER, &fixture.0, "Test"),
            Ok(false)
        );
        for (value, expected) in [(0u32, Ok(false)), (1, Ok(true)), (2, Err(()))] {
            // SAFETY: fixtureキーへ4bytesのDWORDだけを書く。
            assert_eq!(
                unsafe {
                    RegSetValueExW(
                        key.0,
                        name.as_ptr(),
                        0,
                        REG_DWORD,
                        &value as *const u32 as *const u8,
                        4,
                    )
                },
                0
            );
            assert_eq!(
                deletion_registry::read_in(HKEY_CURRENT_USER, &fixture.0, "Test").map_err(|_| ()),
                expected
            );
        }
        let text = wide("1");
        // SAFETY: fixtureキーへNUL終端文字列だけを書く（型不一致fixture）。
        assert_eq!(
            unsafe {
                RegSetValueExW(
                    key.0,
                    name.as_ptr(),
                    0,
                    REG_SZ,
                    text.as_ptr() as *const u8,
                    (text.len() * 2) as u32,
                )
            },
            0
        );
        assert!(deletion_registry::read_in(HKEY_CURRENT_USER, &fixture.0, "Test").is_err());
        drop(key);
        assert!(deletion_registry::read_in(HKEY_CURRENT_USER, &fixture.0, "missing").is_err());
        for name in ["", "a/b", r"a\b", "a\0b"] {
            assert!(deletion_registry::read_in(HKEY_CURRENT_USER, &fixture.0, name).is_err());
        }
        assert!(deletion_registry::classify_value(5, REG_DWORD, 4, 0).is_err());
        assert!(deletion_registry::classify_value(0, REG_DWORD, 3, 0).is_err());
    }

    #[test]
    fn only_the_os_number_1060_means_not_installed() {
        assert_eq!(
            classify_open_failure(Some(1060), "x".to_string()),
            UninstallError::NotInstalled
        );
        // 文字列に 1060 と書いてあっても、番号が違えば（無ければ）未登録にしない。
        assert_eq!(
            classify_open_failure(Some(5), "(os error 1060)".to_string()),
            UninstallError::Open("(os error 1060)".to_string())
        );
        assert_eq!(
            classify_open_failure(None, "os error 1060".to_string()),
            UninstallError::Open("os error 1060".to_string())
        );
    }

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
