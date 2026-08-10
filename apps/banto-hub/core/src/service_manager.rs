//! T17-0（docs/banto-hub-t17-design.md §3「T17-0」・§4「T16-2 への引き渡し
//! 契約」）: SCM（Service Control Manager）状態取得＋start/stop/restart/
//! autostart 操作を、`bin/banto_hub/win_service.rs` から**ホスト非依存の
//! ロジック層**として抽出したもの。
//!
//! ## この T17-0 で行ったこと・行っていないこと
//!
//! - [`ServiceManager`] trait・[`ScmState`]/[`ServiceStatusSummary`]/
//!   [`ServiceManagerError`]/[`TransitionHandle`] という**ホスト非依存**の
//!   型を新設した - Windows API（`windows-service`クレート）に一切触れない
//!   ので非 Windows（このワークスペースの CI）でも単体テストできる
//!   （[`MockServiceManager`]、このファイル末尾の `tests` モジュール参照）。
//! - Windows 実 SCM 実装（[`WindowsServiceManager`]、`#[cfg(windows)]`）を
//!   同じ trait で提供する - `windows-service`クレートを直接叩くのは
//!   `win_service.rs`とこのファイルの2箇所になったが、サービス名
//!   （[`SERVICE_NAME`]）とサービス起動引数（[`RUN_SERVICE_ARG`]）は
//!   このファイル側の定数を`win_service.rs`が再利用する形にして重複を
//!   避けた。
//! - **既存の`install`/`uninstall`/`run-service`CLI（`win_service.rs`）は
//!   一切変更していない** - 起動種別が常に`AutoStart`+遅延自動開始のまま
//!   なのも含め挙動不変（P4「Demand 化」は T17-4 で扱う、このファイルでは
//!   着手しない）。[`WindowsServiceManager`]は「これから T16-2 等が使う
//!   個別操作 API」であって、既存 CLI 経路を今すぐ置き換えるものではない。
//! - `install`/`uninstall`自体は trait に含めていない（設計 §4 の契約が
//!   要求する最小面は query/start/stop/restart/set_auto_start のみ - 実装
//!   指示のとおり、必須ではない拡張は避けた）。

use std::time::{Duration, Instant};

use thiserror::Error;

/// SCM 上のサービス名。`bin/banto_hub/win_service.rs`の`install`/
/// `uninstall`/`run-service`と[`WindowsServiceManager`]が共通で使う単一の
/// ソース（以前は`win_service.rs`だけが`"BantoHub"`を定義していた）。
pub const SERVICE_NAME: &str = "BantoHub";

/// SCM がサービス開始時に子プロセスへ渡す起動引数
/// （`bin/banto-hub.rs`の`run-service`サブコマンド）。[`WindowsServiceManager`]
/// が起動種別変更時にサービス登録を再構築する際、`win_service.rs::install`と
/// 同じ起動コマンドラインを再現するために使う。
pub const RUN_SERVICE_ARG: &str = "run-service";

/// SCM 上のサービス状態（desktop-plan §9.9 のフォールバック UI が判定する
/// 「サービス: 停止/実行中」「STOP_PENDING」等はこの enum の値そのもの -
/// 具体的な文言はこの enum を消費する側（T16-2）が持つ）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScmState {
    /// サービスが SCM に未登録（`install`未実行、または`uninstall`後）。
    NotInstalled,
    Stopped,
    StartPending,
    StopPending,
    Running,
    /// `Paused`/`PausePending`/`ContinuePending`等、banto-hub が能動的に
    /// 使わない残りの SCM 状態を包括する - `String`は診断用の生の状態名
    /// （`windows-service::service::ServiceState`の`Debug`表現）。
    Other(String),
}

impl std::fmt::Display for ScmState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ScmState::NotInstalled => write!(f, "NotInstalled"),
            ScmState::Stopped => write!(f, "Stopped"),
            ScmState::StartPending => write!(f, "StartPending"),
            ScmState::StopPending => write!(f, "StopPending"),
            ScmState::Running => write!(f, "Running"),
            ScmState::Other(detail) => write!(f, "Other({detail})"),
        }
    }
}

/// [`ServiceManager::query_status`]の戻り値 - 設計 §4 の契約どおり。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceStatusSummary {
    pub state: ScmState,
    /// Windows 起動時に自動開始するか（P4 決定後の既定は`false` - この
    /// フィールド自体の意味は変わらないが、既定値を変える`install`側の
    /// 変更は T17-4 で行う）。
    pub auto_start: bool,
    /// 実行中プロセスの PID（`Running`以外では`None`になりうる）。
    pub pid: Option<u32>,
}

/// SCM 操作の失敗を分類したエラー。設計 §4 の契約に沿い、T16-2 の
/// fallback UI が「権限がない」「見つからない」「タイムアウトした」等を
/// 文言分岐できる粒度にした。
#[derive(Debug, Clone, Error)]
pub enum ServiceManagerError {
    /// サービスが SCM に見つからない（`open_service`失敗、未登録）。
    #[error("banto-hub: サービス '{0}' が見つかりません（未登録の可能性）")]
    NotFound(String),
    /// 権限不足（管理者権限、または P3 の`BantoHub Operators`
    /// メンバーシップが必要）。
    #[error("banto-hub: サービス操作の権限がありません（管理者権限が必要です）")]
    AccessDenied,
    /// [`TransitionHandle::wait_until_settled`]が期限内に目標状態へ到達
    /// できなかった。
    #[error("banto-hub: サービスの状態遷移が {0:?} 以内に完了しませんでした")]
    Timeout(Duration),
    /// 上記に当てはまらないその他のエラー（Windows API のエラー文字列を
    /// そのまま保持する診断用）。
    #[error("banto-hub: サービス操作に失敗しました: {0}")]
    Other(String),
}

/// start/stop/restart が返す、非同期な SCM 状態遷移の薄いハンドル。
///
/// SCM の start/stop 系 API は「遷移を受け付けた」時点で同期的に返るだけで、
/// 実際に`Running`/`Stopped`へ落ち着くまでは`StartPending`/`StopPending`を
/// 経由しうる。[`ServiceManager`] trait 自体は非同期 API を持たせず（過剰
/// 設計を避ける）、このハンドルは「遷移が目指す最終状態」だけを保持する
/// 薄い値型にし、完了待ちが必要な呼び出し側は
/// [`TransitionHandle::wait_until_settled`]で`ServiceManager::query_status`を
/// ポーリングする。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransitionHandle {
    /// 遷移が目指す最終状態（`start()`なら`Running`、`stop()`なら`Stopped`）。
    pub target: ScmState,
}

impl TransitionHandle {
    pub fn new(target: ScmState) -> Self {
        Self { target }
    }

    /// `manager.query_status()`を`poll_interval`間隔で`timeout`まで
    /// ポーリングし、状態が[`Self::target`]に到達したらその
    /// [`ServiceStatusSummary`]を返す。到達しないまま`timeout`を超えたら
    /// [`ServiceManagerError::Timeout`]を返す。
    pub fn wait_until_settled(
        &self,
        manager: &dyn ServiceManager,
        timeout: Duration,
        poll_interval: Duration,
    ) -> Result<ServiceStatusSummary, ServiceManagerError> {
        let deadline = Instant::now() + timeout;
        loop {
            let status = manager.query_status()?;
            if status.state == self.target {
                return Ok(status);
            }
            if Instant::now() >= deadline {
                return Err(ServiceManagerError::Timeout(timeout));
            }
            std::thread::sleep(poll_interval);
        }
    }
}

/// T17-0 が抽出する SCM 操作のロジック層（設計 §4 の契約そのもの）。
///
/// T16-2（サービス検出・native fallback UI）はこの trait だけを消費し、
/// `windows-service`クレートや SCM API を直接呼ばない（P5、設計 §3）。
/// 実装は2つ: 単体テスト用の[`MockServiceManager`]（このファイル、常に
/// 利用可能）と、実 SCM を叩く[`WindowsServiceManager`]
/// （`#[cfg(windows)]`）。
pub trait ServiceManager {
    /// 現在の SCM 状態・自動開始設定・PID を取得する。サービス未登録は
    /// エラーではなく`ScmState::NotInstalled`として返す（設計 §4 の
    /// `ServiceStatusSummary`契約）。
    fn query_status(&self) -> Result<ServiceStatusSummary, ServiceManagerError>;

    /// サービス開始を要求する。**冪等** - 既に`Running`なら SCM へ実際の
    /// start 要求を出さず、現在状態を指す[`TransitionHandle`]を返すだけ
    /// （設計 §4「多重クリック対策」）。
    fn start(&self) -> Result<TransitionHandle, ServiceManagerError>;

    /// サービス停止を要求する。`start`と同様に冪等。
    fn stop(&self) -> Result<TransitionHandle, ServiceManagerError>;

    /// サービスを再起動する。stop→start の合成でよい（設計 §3「stop+start
    /// 合成で可」）— 各実装が`stop`の完了を待ってから`start`する具体的な
    /// 待ち方（[`TransitionHandle::wait_until_settled`]の利用有無）を決める。
    fn restart(&self) -> Result<TransitionHandle, ServiceManagerError>;

    /// 自動開始（Windows 起動時にサービスを開始するか）を切り替える。
    fn set_auto_start(&self, enabled: bool) -> Result<(), ServiceManagerError>;
}

/// [`ServiceManager`]のインメモリモック実装 - 単体テスト用
/// （このファイル末尾の`tests`モジュール、および将来 T16-2 側のテストが
/// 消費してもよい）。実 SCM の`StartPending`/`StopPending`のような中間
/// 状態は再現しない（過剰設計を避ける - 遷移は`start`/`stop`呼び出し内で
/// 即座に完結する）。
pub struct MockServiceManager {
    state: std::sync::Mutex<MockState>,
}

struct MockState {
    installed: bool,
    scm_state: ScmState,
    auto_start: bool,
    pid: Option<u32>,
    /// 次の1回の呼び出しでだけ返すエラー（[`MockServiceManager::inject_error`]
    /// で設定、消費後は自動的にクリアされる - 実運用の一時的な失敗を再現
    /// するテスト用フック）。
    next_error: Option<ServiceManagerError>,
}

/// モックが`Running`時に返す固定 PID（実プロセスは存在しないので実際の
/// PID には意味を持たせず、`Some`であること自体だけを検証対象にする）。
const MOCK_PID: u32 = 4242;

impl MockServiceManager {
    /// 登録済み・`Stopped`・自動開始 OFF の状態で始まる（P4 決定後の
    /// `install`既定に近い状態 - T17-0 時点ではまだ`install`側の既定は
    /// `AutoStart`のままだが、モックの既定はテストの書きやすさを優先し
    /// `Stopped`にしている）。
    pub fn new() -> Self {
        Self {
            state: std::sync::Mutex::new(MockState {
                installed: true,
                scm_state: ScmState::Stopped,
                auto_start: false,
                pid: None,
                next_error: None,
            }),
        }
    }

    /// 未登録（`uninstall`後、または`install`前）状態で始まるモック。
    pub fn not_installed() -> Self {
        Self {
            state: std::sync::Mutex::new(MockState {
                installed: false,
                scm_state: ScmState::NotInstalled,
                auto_start: false,
                pid: None,
                next_error: None,
            }),
        }
    }

    /// 次の1回の trait 呼び出しでだけ返すエラーを設定する（テストの
    /// 「権限エラーで start が失敗する」等のシナリオ用）。
    pub fn inject_error(&self, err: ServiceManagerError) {
        self.state
            .lock()
            .expect("mock state mutex poisoned")
            .next_error = Some(err);
    }

    /// 現在の状態を直接読む（アサーション用のテストヘルパー）。
    pub fn current_state(&self) -> ScmState {
        self.state
            .lock()
            .expect("mock state mutex poisoned")
            .scm_state
            .clone()
    }

    fn take_injected_error(state: &mut MockState) -> Option<ServiceManagerError> {
        state.next_error.take()
    }
}

impl Default for MockServiceManager {
    fn default() -> Self {
        Self::new()
    }
}

impl ServiceManager for MockServiceManager {
    fn query_status(&self) -> Result<ServiceStatusSummary, ServiceManagerError> {
        let mut state = self.state.lock().expect("mock state mutex poisoned");
        if let Some(err) = Self::take_injected_error(&mut state) {
            return Err(err);
        }
        Ok(ServiceStatusSummary {
            state: state.scm_state.clone(),
            auto_start: state.auto_start,
            pid: state.pid,
        })
    }

    fn start(&self) -> Result<TransitionHandle, ServiceManagerError> {
        let mut state = self.state.lock().expect("mock state mutex poisoned");
        if let Some(err) = Self::take_injected_error(&mut state) {
            return Err(err);
        }
        if !state.installed {
            return Err(ServiceManagerError::NotFound(SERVICE_NAME.to_string()));
        }
        // 冪等（設計 §4「多重クリック対策」）: 既に Running ならそのまま。
        if state.scm_state != ScmState::Running {
            state.scm_state = ScmState::Running;
            state.pid = Some(MOCK_PID);
        }
        Ok(TransitionHandle::new(ScmState::Running))
    }

    fn stop(&self) -> Result<TransitionHandle, ServiceManagerError> {
        let mut state = self.state.lock().expect("mock state mutex poisoned");
        if let Some(err) = Self::take_injected_error(&mut state) {
            return Err(err);
        }
        if !state.installed {
            return Err(ServiceManagerError::NotFound(SERVICE_NAME.to_string()));
        }
        if state.scm_state != ScmState::Stopped {
            state.scm_state = ScmState::Stopped;
            state.pid = None;
        }
        Ok(TransitionHandle::new(ScmState::Stopped))
    }

    fn restart(&self) -> Result<TransitionHandle, ServiceManagerError> {
        // モックは中間状態を再現しないため、実 SCM 実装のように
        // `wait_until_settled`を挟まず stop→start をそのまま合成できる
        // （設計 §3「stop+start 合成で可」）。
        self.stop()?;
        self.start()
    }

    fn set_auto_start(&self, enabled: bool) -> Result<(), ServiceManagerError> {
        let mut state = self.state.lock().expect("mock state mutex poisoned");
        if let Some(err) = Self::take_injected_error(&mut state) {
            return Err(err);
        }
        if !state.installed {
            return Err(ServiceManagerError::NotFound(SERVICE_NAME.to_string()));
        }
        state.auto_start = enabled;
        Ok(())
    }
}

/// Windows 実 SCM を叩く[`ServiceManager`]実装（T17-0 スコープ）。
///
/// `win_service.rs`の`install`/`uninstall`とは独立した経路 -
/// 既存 CLI はこの型を経由せず、従来どおり`windows-service`クレートを
/// 直接呼ぶ（このファイル冒頭のモジュール doc 参照）。
///
/// ## `set_auto_start`の制約
///
/// SCM の`ChangeServiceConfigW`（`windows-service`クレートの
/// `Service::change_config`）は変更しないフィールドも含めて`ServiceInfo`
/// 全体を渡す必要があり、かつ`executable_path`/`launch_arguments`は
/// 呼び出し側が渡した生のパス・引数から SCM 用のコマンドラインへ**その場で
/// エスケープ**される（`windows-service`クレートの`escape_wide`）。
/// そのため、`query_config`で取得した「既にエスケープ済みの生コマンド
/// ライン文字列」を`executable_path`にそのまま渡すと二重エスケープで
/// 壊れる。安全のため[`WindowsServiceManager`]は`win_service.rs::install`と
/// 同じ組み立て方（[`executable_path`][Self::executable_path] + 固定引数
/// [`RUN_SERVICE_ARG`]）で`ServiceInfo`を再構築する - 呼び出し時点で
/// **実際にサービスへ登録されている実行ファイルパスと`executable_path`が
/// 一致している**ことが前提（T17-0 時点の唯一の呼び出し元は
/// console/service ホスト自身であり、これは常に成り立つ。T16-2 が別プロセス
/// から呼ぶ場合は、正しい headless exe パスを構築して渡す責務を呼び出し側が
/// 持つ）。
#[cfg(windows)]
pub struct WindowsServiceManager {
    /// `set_auto_start`がサービス再登録時に使う実行ファイルパス（上記
    /// 制約参照）。
    executable_path: std::path::PathBuf,
}

#[cfg(windows)]
mod windows_impl {
    use std::ffi::OsString;
    use std::path::PathBuf;

    use windows_service::service::{
        Service, ServiceAccess, ServiceErrorControl, ServiceInfo, ServiceStartType, ServiceState,
        ServiceType,
    };
    use windows_service::service_manager::{
        ServiceManager as WinScm, ServiceManagerAccess as WinScmAccess,
    };

    use super::{
        ScmState, ServiceManager, ServiceManagerError, ServiceStatusSummary, TransitionHandle,
        WindowsServiceManager, RUN_SERVICE_ARG, SERVICE_NAME,
    };

    /// `win_service.rs`の`SERVICE_DISPLAY_NAME`/`SERVICE_TYPE`と同じ値
    /// （このモジュール冒頭の doc「`set_auto_start`の制約」参照 -
    /// 再登録時に`win_service.rs::install`と同じ`ServiceInfo`を再現する
    /// ために必要）。値を変えるときは両ファイルを同時に直すこと。
    const SERVICE_DISPLAY_NAME: &str = "banto-hub タグサーバー";
    const SERVICE_TYPE: ServiceType = ServiceType::OWN_PROCESS;

    /// Win32 エラーコード（`ERROR_SERVICE_DOES_NOT_EXIST`）- サービス未登録
    /// の判定に使う。`windows-sys`の対応する定数へ依存を増やさず、
    /// 文書化された固定値をそのまま使う。
    const ERROR_SERVICE_DOES_NOT_EXIST: i32 = 1060;
    /// Win32 エラーコード（`ERROR_ACCESS_DENIED`）。
    const ERROR_ACCESS_DENIED: i32 = 5;

    impl WindowsServiceManager {
        /// `executable_path`は`set_auto_start`での再登録に使う実行ファイル
        /// パス（構造体 doc 参照）。通常は`std::env::current_exe()`を渡す。
        pub fn new(executable_path: PathBuf) -> Self {
            Self { executable_path }
        }
    }

    fn map_win_err(err: windows_service::Error) -> ServiceManagerError {
        if let windows_service::Error::Winapi(io_err) = &err {
            match io_err.raw_os_error() {
                Some(ERROR_SERVICE_DOES_NOT_EXIST) => {
                    return ServiceManagerError::NotFound(SERVICE_NAME.to_string());
                }
                Some(ERROR_ACCESS_DENIED) => return ServiceManagerError::AccessDenied,
                _ => {}
            }
        }
        ServiceManagerError::Other(err.to_string())
    }

    fn open_scm(access: WinScmAccess) -> Result<WinScm, ServiceManagerError> {
        WinScm::local_computer(None::<&str>, access).map_err(map_win_err)
    }

    /// `open_service`失敗を「未登録」と「その他のエラー」に分ける共通処理。
    /// 未登録は[`ServiceManagerError`]ではなく`Ok(None)`として返し、呼び出し
    /// 側（`query_status`）が`ScmState::NotInstalled`へマップする（設計 §4
    /// の契約「サービス未登録はエラーではない」）。
    fn try_open_service(
        manager: &WinScm,
        access: ServiceAccess,
    ) -> Result<Option<Service>, ServiceManagerError> {
        match manager.open_service(SERVICE_NAME, access) {
            Ok(service) => Ok(Some(service)),
            Err(err) => {
                if let windows_service::Error::Winapi(io_err) = &err {
                    if io_err.raw_os_error() == Some(ERROR_SERVICE_DOES_NOT_EXIST) {
                        return Ok(None);
                    }
                }
                Err(map_win_err(err))
            }
        }
    }

    fn map_state(state: ServiceState) -> ScmState {
        match state {
            ServiceState::Stopped => ScmState::Stopped,
            ServiceState::StartPending => ScmState::StartPending,
            ServiceState::StopPending => ScmState::StopPending,
            ServiceState::Running => ScmState::Running,
            other => ScmState::Other(format!("{other:?}")),
        }
    }

    impl ServiceManager for WindowsServiceManager {
        fn query_status(&self) -> Result<ServiceStatusSummary, ServiceManagerError> {
            let manager = open_scm(WinScmAccess::CONNECT)?;
            let access = ServiceAccess::QUERY_STATUS | ServiceAccess::QUERY_CONFIG;
            let service = match try_open_service(&manager, access)? {
                Some(service) => service,
                None => {
                    return Ok(ServiceStatusSummary {
                        state: ScmState::NotInstalled,
                        auto_start: false,
                        pid: None,
                    });
                }
            };
            let status = service.query_status().map_err(map_win_err)?;
            let config = service.query_config().map_err(map_win_err)?;
            Ok(ServiceStatusSummary {
                state: map_state(status.current_state),
                auto_start: matches!(config.start_type, ServiceStartType::AutoStart),
                pid: status.process_id,
            })
        }

        fn start(&self) -> Result<TransitionHandle, ServiceManagerError> {
            let manager = open_scm(WinScmAccess::CONNECT)?;
            let access = ServiceAccess::START | ServiceAccess::QUERY_STATUS;
            let service = try_open_service(&manager, access)?
                .ok_or_else(|| ServiceManagerError::NotFound(SERVICE_NAME.to_string()))?;
            let status = service.query_status().map_err(map_win_err)?;
            // 冪等（設計 §4「多重クリック対策」）。
            if status.current_state != ServiceState::Running {
                service.start::<&str>(&[]).map_err(map_win_err)?;
            }
            Ok(TransitionHandle::new(ScmState::Running))
        }

        fn stop(&self) -> Result<TransitionHandle, ServiceManagerError> {
            let manager = open_scm(WinScmAccess::CONNECT)?;
            let access = ServiceAccess::STOP | ServiceAccess::QUERY_STATUS;
            let service = try_open_service(&manager, access)?
                .ok_or_else(|| ServiceManagerError::NotFound(SERVICE_NAME.to_string()))?;
            let status = service.query_status().map_err(map_win_err)?;
            if status.current_state != ServiceState::Stopped {
                service.stop().map_err(map_win_err)?;
            }
            Ok(TransitionHandle::new(ScmState::Stopped))
        }

        fn restart(&self) -> Result<TransitionHandle, ServiceManagerError> {
            // stop の完了を待ってから start する（設計 §3「stop+start 合成
            // で可」）。実 SCM は`StopPending`を経由しうるため、モックとは
            // 違いここでは実際に`wait_until_settled`でポーリングする。
            let stop_handle = self.stop()?;
            stop_handle.wait_until_settled(
                self,
                std::time::Duration::from_secs(30),
                std::time::Duration::from_millis(200),
            )?;
            self.start()
        }

        fn set_auto_start(&self, enabled: bool) -> Result<(), ServiceManagerError> {
            let manager = open_scm(WinScmAccess::CONNECT)?;
            let access = ServiceAccess::CHANGE_CONFIG;
            let service = try_open_service(&manager, access)?
                .ok_or_else(|| ServiceManagerError::NotFound(SERVICE_NAME.to_string()))?;
            let start_type = if enabled {
                ServiceStartType::AutoStart
            } else {
                ServiceStartType::OnDemand
            };
            // このファイル冒頭のモジュール doc「`set_auto_start`の制約」
            // 参照 - `win_service.rs::install`と同じ組み立て方で再構築する。
            let service_info = ServiceInfo {
                name: OsString::from(SERVICE_NAME),
                display_name: OsString::from(SERVICE_DISPLAY_NAME),
                service_type: SERVICE_TYPE,
                start_type,
                error_control: ServiceErrorControl::Normal,
                executable_path: self.executable_path.clone(),
                launch_arguments: vec![OsString::from(RUN_SERVICE_ARG)],
                dependencies: vec![],
                account_name: None,
                account_password: None,
            };
            service.change_config(&service_info).map_err(map_win_err)?;
            if enabled {
                // install()と同じ判断（遅延自動開始、win_service.rs の
                // モジュール doc 参照）を autostart 再有効化時にも適用する -
                // P4（Demand 化）が入るまでは AutoStart=遅延あり、
                // OnDemand=該当なし、の対応を保つ。
                service.set_delayed_auto_start(true).map_err(map_win_err)?;
            }
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_status_not_installed_by_default_helper() {
        let manager = MockServiceManager::not_installed();
        let status = manager.query_status().expect("query_status should succeed");
        assert_eq!(status.state, ScmState::NotInstalled);
        assert!(!status.auto_start);
        assert_eq!(status.pid, None);
    }

    #[test]
    fn query_status_new_defaults_to_stopped() {
        let manager = MockServiceManager::new();
        let status = manager.query_status().expect("query_status should succeed");
        assert_eq!(status.state, ScmState::Stopped);
        assert!(!status.auto_start);
        assert_eq!(status.pid, None);
    }

    #[test]
    fn start_transitions_stopped_to_running_with_pid() {
        let manager = MockServiceManager::new();
        let handle = manager.start().expect("start should succeed");
        assert_eq!(handle.target, ScmState::Running);

        let status = manager.query_status().expect("query_status should succeed");
        assert_eq!(status.state, ScmState::Running);
        assert!(status.pid.is_some());
    }

    #[test]
    fn start_is_idempotent_when_already_running() {
        let manager = MockServiceManager::new();
        manager.start().expect("first start should succeed");
        let pid_after_first_start = manager.query_status().unwrap().pid;

        // 2回目の start は「既に Running」を無視して何もしない
        // （設計 §4「多重クリック対策」）- PID が変わらないことで検証する。
        manager.start().expect("second start should be a no-op");
        let pid_after_second_start = manager.query_status().unwrap().pid;
        assert_eq!(pid_after_first_start, pid_after_second_start);
    }

    #[test]
    fn stop_transitions_running_to_stopped_and_clears_pid() {
        let manager = MockServiceManager::new();
        manager.start().expect("start should succeed");
        let handle = manager.stop().expect("stop should succeed");
        assert_eq!(handle.target, ScmState::Stopped);

        let status = manager.query_status().expect("query_status should succeed");
        assert_eq!(status.state, ScmState::Stopped);
        assert_eq!(status.pid, None);
    }

    #[test]
    fn stop_is_idempotent_when_already_stopped() {
        let manager = MockServiceManager::new();
        // 開始前（Stopped）に stop しても失敗しない。
        let handle = manager
            .stop()
            .expect("stop on already-stopped should succeed");
        assert_eq!(handle.target, ScmState::Stopped);
    }

    #[test]
    fn restart_ends_up_running() {
        let manager = MockServiceManager::new();
        manager.start().expect("start should succeed");
        let handle = manager.restart().expect("restart should succeed");
        assert_eq!(handle.target, ScmState::Running);
        assert_eq!(manager.current_state(), ScmState::Running);
    }

    #[test]
    fn set_auto_start_toggles_flag() {
        let manager = MockServiceManager::new();
        assert!(!manager.query_status().unwrap().auto_start);

        manager
            .set_auto_start(true)
            .expect("set_auto_start should succeed");
        assert!(manager.query_status().unwrap().auto_start);

        manager
            .set_auto_start(false)
            .expect("set_auto_start should succeed");
        assert!(!manager.query_status().unwrap().auto_start);
    }

    #[test]
    fn operations_on_not_installed_return_not_found() {
        let manager = MockServiceManager::not_installed();
        assert!(matches!(
            manager.start(),
            Err(ServiceManagerError::NotFound(_))
        ));
        assert!(matches!(
            manager.stop(),
            Err(ServiceManagerError::NotFound(_))
        ));
        assert!(matches!(
            manager.set_auto_start(true),
            Err(ServiceManagerError::NotFound(_))
        ));
    }

    #[test]
    fn injected_error_is_returned_once_then_clears() {
        let manager = MockServiceManager::new();
        manager.inject_error(ServiceManagerError::AccessDenied);

        assert!(matches!(
            manager.start(),
            Err(ServiceManagerError::AccessDenied)
        ));
        // 1回消費したら通常の挙動に戻る。
        manager
            .start()
            .expect("second start should succeed after error is consumed");
        assert_eq!(manager.current_state(), ScmState::Running);
    }

    #[test]
    fn transition_handle_wait_until_settled_returns_once_state_matches() {
        let manager = MockServiceManager::new();
        // モックは即座に遷移が完結するので、poll 1回目で成功するはず。
        let handle = manager.start().expect("start should succeed");
        let status = handle
            .wait_until_settled(&manager, Duration::from_secs(1), Duration::from_millis(1))
            .expect("wait_until_settled should observe the already-settled state");
        assert_eq!(status.state, ScmState::Running);
    }

    #[test]
    fn transition_handle_wait_until_settled_times_out_when_target_never_reached() {
        let manager = MockServiceManager::new();
        // target を実際には到達しない状態にして timeout 経路を検証する。
        let handle = TransitionHandle::new(ScmState::Running);
        manager.stop().expect("stop should succeed");
        let result = handle.wait_until_settled(
            &manager,
            Duration::from_millis(20),
            Duration::from_millis(5),
        );
        assert!(matches!(result, Err(ServiceManagerError::Timeout(_))));
    }

    #[test]
    fn scm_state_display_matches_variant_name() {
        assert_eq!(ScmState::NotInstalled.to_string(), "NotInstalled");
        assert_eq!(ScmState::Running.to_string(), "Running");
        assert_eq!(
            ScmState::Other("Paused".to_string()).to_string(),
            "Other(Paused)"
        );
    }
}
