//! SCM（Windows Service Control Manager）の状態取得と start / stop /
//! restart / 自動開始の切り替えを、**サービス名を引数で受ける形**で提供する
//! 共有 crate。banto-hub（`BantoHub`）・banto-hub-sink（`BantoHubSink`）・
//! ChronoGazer のサービス化（#392 段階 B）が同じ型で SCM を扱う。
//!
//! ## 設計意図
//!
//! - [`ServiceManager`] trait・[`ScmState`]/[`ServiceStatusSummary`]/
//!   [`ServiceManagerError`]/[`TransitionHandle`] は**ホスト非依存**の型で、
//!   Windows API に触れない。消費側（トレイのサービス切替・fallback UI 等）
//!   はこの trait だけを呼ぶ。非 Windows の CI でも
//!   [`MockServiceManager`]（常に利用可能）で単体テストできる。
//! - 実 SCM を叩く [`WindowsServiceManager`]（`#[cfg(windows)]`、
//!   `windows-service`クレート）は [`WindowsServiceManager::for_service`] で
//!   サービス名・表示名・起動引数・実行ファイルパスを呼び出し側から受ける。
//!   **製品の知識（既定のサービス名・起動引数など）はこの crate に持たせない**
//!   — それらを束ねる薄いラッパー（banto-hub の`BantoHub`向け等）は製品側
//!   crate が持つ。
//! - [`ServiceManager`] trait の最小面は query / start / stop / restart /
//!   set_auto_start だけ。登録・解除は trait に入れず、別の手順
//!   （下の `install`）として持つ。
//! - **サービス本体の側**（#392 B3-1）: `runner`（[`run_service`]・
//!   [`ServiceProgram`]・[`ServiceContext`]・[`ServiceExit`]）が SCM との
//!   やりとり（ディスパッチャ、制御ハンドラ、STOP と PRESHUTDOWN、
//!   Running → StopPending → Stopped の報告、本体の panic）を 1 か所で行う。
//!   製品はログの開き方と本体と終了コードの意味だけを持つ。
//! - **登録・解除**（#392 B3-1）: `install`（[`ServiceSpec`]・
//!   [`install_service`]・[`uninstall_service`]）が SCM の手順を持ち、結果を
//!   値で返す（表示もプロセスの終了もしない - 利用者に見える文言と終了
//!   コードは製品が持つ）。既に登録済みなら起動種別・実行ファイル・ACL は
//!   変えず、OS シャットダウンの待ち時間（`shutdown` の
//!   [`ensure_preshutdown_timeout`]、#589）だけを揃える。
//! - **サービスごとの設定値**（#392 B3-2a）: `parameters`
//!   （`write_service_parameter`・`read_service_parameter`、Windows だけ）が
//!   `HKLM\SYSTEM\CurrentControlSet\Services\<service>\Parameters` の REG_SZ を
//!   読み書きする。登録時に記録してサービス本体が起動時に読む値（ChronoGazer の
//!   インストール利用者の SID）のため。値の名前と意味は製品が持つ。
//!
//! ## 履歴
//!
//! banto-hub-core の `service_manager.rs`（T17-0、docs/banto-hub-t17-design.md
//! §3・§4。S6 でサービス名をコンストラクタ引数へ持ち上げ済み）から、#392
//! 段階 B の B2-3 で抽出した。挙動は変えていない（banto-hub-core は
//! `service_manager`モジュールでこの crate を再公開し、`BantoHub`/
//! `BantoHubSink`の定数と`BantoHub`向けのコンストラクタだけを持つ）。差は
//! [`MockServiceManager`]が`NotFound`に載せる既定のサービス名が
//! 製品名ではない中立な値になったこと（[`MockServiceManager::with_service_name`]
//! で差し替えられる）だけ。B2-4a で[`ServiceManagerError`]の`Display`を
//! 製品名を含まない中立な文言にした（banto-hub の従来の文言は
//! banto-hub-core の`describe_service_error`が組み立てる）。
//!
//! B3-1 で、banto-hub（`bin/banto_hub/win_service.rs`・`service_install.rs`）と
//! banto-hub-sink（`service.rs`）に写しで並んでいたサービス本体のランナーと
//! 登録・解除の手順をここへ移した（`runner`・`install`）。両製品は
//! [`ServiceProgram`] を実装して [`run_service`] を呼び、install/uninstall の
//! 文言は移す前と 1 バイトも変えていない（各製品の golden テスト）。差は
//! 本体の panic で Stopped（`ServiceSpecific(1)`）を報告するようになった
//! こと（2026-10-15 オーナー決定 D2）と、手動で `run-service` を実行した
//! ときの案内の文言を製品共通にしたこと（同 D3、標準エラー出力だけ）。

use std::time::{Duration, Instant};

use thiserror::Error;

mod install;
mod parameters;
mod runner;
mod shutdown;
pub use install::*;
// 公開しているのは Windows だけの関数（非 Windows では空の再公開になり、
// `unused_imports` で clippy が落ちる）。
#[cfg(windows)]
pub use parameters::*;
pub use runner::*;
pub use shutdown::*;

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
    /// Windows 起動時に自動開始するか（T17-4 で`install`の既定を
    /// `OnDemand`（`false`相当）に変更済み - このフィールド自体の意味は
    /// 変わらない）。
    pub auto_start: bool,
    /// 実行中プロセスの PID（`Running`以外では`None`になりうる）。
    pub pid: Option<u32>,
    /// SCM の `dwCheckPoint`。`StartPending`／`StopPending` の間、サービスが
    /// 進むたびに増える（`pending_progress`で「進んでいる」判断に使う）。
    /// pending でなければ 0。
    pub checkpoint: u32,
    /// SCM の `dwWaitHint`。pending の間の、次の進みまでの見込み。
    /// pending でなければ `Duration::ZERO`。
    pub wait_hint: Duration,
    /// SCM の `dwWin32ExitCode`。正常は 0、サービス固有の終了コードで
    /// 止まったときは `ERROR_SERVICE_SPECIFIC_ERROR`（1066）で、そのとき
    /// 値は [`Self::service_specific_exit_code`] にある。未登録なら 0。
    pub win32_exit_code: u32,
    /// SCM の `dwServiceSpecificExitCode`。`win32_exit_code` が 1066 の
    /// ときだけ意味を持つ（`relax-only` の 0／5 など、サービスが自分で
    /// 返した終了コード）。未登録なら 0。
    pub service_specific_exit_code: u32,
}

/// 2 回の照会から、pending（起動中・停止中）が**進んでいる**かを決める
/// 純関数（シェルが「進んでいる間は待つ」判断に使う）。
///
/// 同じ pending の状態のまま `checkpoint` が増えていれば進んでいる。状態が
/// 変わった（`previous` が pending でなかった）ときは、新しい pending の
/// 始まりなので進んでいる扱い。`checkpoint` が増えない・戻ったときは止まって
/// いる（期限を延ばさない）。pending でない `current` は `false`
/// （待つ必要が無い）。
///
/// 2 回の照会の間にサービスが再起動して**同じ pending に戻った**とき
/// （StartPending 5 → Running → StartPending 2）は「戻った」と見て `false`
/// を返す。まれで、起きても期限を延ばさないだけの安全な側。
pub fn pending_progress(
    previous: Option<&ServiceStatusSummary>,
    current: &ServiceStatusSummary,
) -> bool {
    let is_pending = |s: &ScmState| matches!(s, ScmState::StartPending | ScmState::StopPending);
    if !is_pending(&current.state) {
        return false;
    }
    match previous {
        Some(prev) if prev.state == current.state => current.checkpoint > prev.checkpoint,
        _ => true,
    }
}

/// SCM 操作の失敗を分類したエラー。設計 §4 の契約に沿い、T16-2 の
/// fallback UI が「権限がない」「見つからない」「タイムアウトした」等を
/// 文言分岐できる粒度にした。
///
/// `Display`の文言は製品名を含まない中立な形（#392 B2-4a）。B2-3 の抽出
/// 時点では banto-hub の文言（先頭の`banto-hub: `、権限不足の
/// 「（管理者権限が必要です）」）のままだったが、ChronoGazer も同じ型を
/// 使うため中立にした。banto-hub の利用者に見える文言は
/// `banto_hub_core::service_manager::describe_service_error`が従来どおりに
/// 組み立てる。
#[derive(Debug, Clone, Error)]
pub enum ServiceManagerError {
    /// サービスが SCM に見つからない（`open_service`失敗、未登録）。
    #[error("サービス '{0}' が見つかりません（未登録の可能性）")]
    NotFound(String),
    /// 権限不足（管理者権限、または製品ごとの Operators グループ
    /// （banto-hub なら P3 の`BantoHub Operators`）のメンバーシップが必要）。
    /// 文言は「管理者」を名指ししない（Operators で足りる製品があるため。
    /// 製品固有の案内は消費側が組み立てる - banto-hub は
    /// `banto_hub_core::service_manager::describe_service_error`）。
    #[error("サービス操作の権限がありません")]
    AccessDenied,
    /// [`TransitionHandle::wait_until_settled`]が期限内に目標状態へ到達
    /// できなかった。
    #[error("サービスの状態遷移が {0:?} 以内に完了しませんでした")]
    Timeout(Duration),
    /// 上記に当てはまらないその他のエラー（Windows API のエラー文字列を
    /// そのまま保持する診断用）。
    #[error("サービス操作に失敗しました: {0}")]
    Other(String),
    /// 要求できる状態ではなかった（引数つきの開始は `Stopped` のときだけ。
    /// 値は照会した現在の状態）。要求は出していない。
    #[error("サービスは {0} のため、引数つきの開始は要求できません（停止中のときだけ）")]
    InvalidState(ScmState),
    /// 開始を出したが、サービスは既に開始していた（`ERROR_SERVICE_ALREADY_RUNNING`、
    /// 1056。照会と開始の間に別の操作が入った。#623 PR-4）。
    #[error("サービスは既に開始しています")]
    AlreadyRunning,
    /// サービスが今その制御を受け付けない（`ERROR_SERVICE_CANNOT_ACCEPT_CTRL`、1061。
    /// 開始の途中（START_PENDING）の STOP など。呼び出し側は失敗にせず待って出し直せる。
    /// #623 PR-4）。
    #[error("サービスは開始や停止の途中のため、今はこの操作を受け付けられません")]
    CannotAcceptControl,
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

/// SCM 操作のロジック層（banto-hub の T17-0 設計 §4 の契約そのもの）。
///
/// 消費側（banto-hub のサービス検出・native fallback UI、ChronoGazer）は
/// この trait だけを消費し、`windows-service`クレートや SCM API を直接
/// 呼ばない。実装は2つ: 単体テスト用の[`MockServiceManager`]（常に利用
/// 可能）と、実 SCM を叩く[`WindowsServiceManager`]（`#[cfg(windows)]`）。
pub trait ServiceManager {
    /// 現在の SCM 状態・自動開始設定・PID を取得する。サービス未登録は
    /// エラーではなく`ScmState::NotInstalled`として返す（設計 §4 の
    /// `ServiceStatusSummary`契約）。
    fn query_status(&self) -> Result<ServiceStatusSummary, ServiceManagerError>;

    /// サービス開始を要求する。**冪等** - 既に`Running`なら SCM へ実際の
    /// start 要求を出さず、現在状態を指す[`TransitionHandle`]を返すだけ
    /// （設計 §4「多重クリック対策」）。
    fn start(&self) -> Result<TransitionHandle, ServiceManagerError>;

    /// 引数を渡してサービス開始を要求する（`StartServiceW` の
    /// `lpServiceArgVectors`。サービスは `ServiceMain` の argv の `[1..]` で
    /// 受け取る - `ServiceContext::start_args`）。Operators の権利
    /// （`START`と`QUERY_STATUS`）だけで呼べる。
    ///
    /// 引数が**空**なら[`Self::start`]と同じ（冪等）。引数があるときは
    /// **`Stopped` のときだけ**要求する。`Running`・pending など他の状態では
    /// 要求を出さず[`ServiceManagerError::InvalidState`]で断る（[`Self::start`]
    /// のような冪等な `Ok` にすると、引数が渡らなかったのに成功に見える）。
    /// 照会と開始の間に別の操作が入った場合は、SCM が開始を断る
    /// （`ERROR_SERVICE_ALREADY_RUNNING` など）。
    ///
    /// **返る[`TransitionHandle`]の目標は `Running` 固定**で、引数が Running
    /// まで進む開始のときだけ `wait_until_settled` で待てる。`relax-only` の
    /// ように Running を報告せず START_PENDING から STOPPED へ進む引数では
    /// `Running` に着かないので毎回 `Timeout` になる。その完了は
    /// `query_status` を自分でポーリングし、`StartPending` を通った後の
    /// `Stopped` と終了コード（[`ServiceStatusSummary::win32_exit_code`]・
    /// [`ServiceStatusSummary::service_specific_exit_code`]）で見る
    /// （開始の直前の `Stopped` を「着いた」と取り違えないこと）。
    ///
    /// 引数は**信頼できない入力**として扱う: `START` の権利があれば
    /// （Operators でも）誰でも任意の語・個数を渡せる。受ける側（サービス）
    /// の決まった値だけを受け付ける責任は、`ServiceContext::start_args` の
    /// 側にある。
    fn start_with_args(
        &self,
        args: &[std::ffi::OsString],
    ) -> Result<TransitionHandle, ServiceManagerError>;

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
/// （この crate 末尾の`tests`モジュール、および各製品側のテストが消費する）。実 SCM の`StartPending`/`StopPending`のような中間
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
    /// `NotFound`に載せるサービス名（[`MockServiceManager::with_service_name`]）。
    service_name: String,
    /// 最後に実際に開始した（`Running`にした）ときの引数
    /// （[`MockServiceManager::last_start_args`]）。
    last_start_args: Vec<std::ffi::OsString>,
    /// 照会に載せる pending の進み（[`MockServiceManager::set_pending_progress`]）。
    checkpoint: u32,
    wait_hint: Duration,
}

/// モックが`Running`時に返す固定 PID（実プロセスは存在しないので実際の
/// PID には意味を持たせず、`Some`であること自体だけを検証対象にする）。
const MOCK_PID: u32 = 4242;

/// [`MockServiceManager::new`]/[`MockServiceManager::not_installed`]が
/// `NotFound`に載せる既定のサービス名（製品名を持たない中立な値。製品側の
/// テストで名前を見たいときは[`MockServiceManager::with_service_name`]）。
const MOCK_SERVICE_NAME: &str = "mock-service";

impl MockServiceManager {
    /// 登録済み・`Stopped`・自動開始 OFF の状態で始まる（T17-4 で
    /// `install`側の既定も`OnDemand`（自動開始 OFF 相当）になり、この
    /// モックの既定と一致した。`Stopped`にしているのはテストの書きやすさ
    /// を優先したもの）。
    pub fn new() -> Self {
        Self {
            state: std::sync::Mutex::new(MockState {
                installed: true,
                scm_state: ScmState::Stopped,
                auto_start: false,
                pid: None,
                next_error: None,
                service_name: MOCK_SERVICE_NAME.to_string(),
                last_start_args: Vec::new(),
                checkpoint: 0,
                wait_hint: Duration::ZERO,
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
                service_name: MOCK_SERVICE_NAME.to_string(),
                last_start_args: Vec::new(),
                checkpoint: 0,
                wait_hint: Duration::ZERO,
            }),
        }
    }

    /// `NotFound`に載せるサービス名を差し替える（未登録モックの操作が返す
    /// エラーの文言を、対象の製品のサービス名にしたいテスト用）。
    pub fn with_service_name(self, service_name: impl Into<String>) -> Self {
        self.state
            .lock()
            .expect("mock state mutex poisoned")
            .service_name = service_name.into();
        self
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

    /// 最後に実際に開始した（`Running`にした）ときの引数
    /// （[`ServiceManager::start_with_args`]。`start`は空）。
    pub fn last_start_args(&self) -> Vec<std::ffi::OsString> {
        self.state
            .lock()
            .expect("mock state mutex poisoned")
            .last_start_args
            .clone()
    }

    /// 状態を設定し、照会に載せる checkpoint と wait hint を設定する
    /// （シェルの「進んでいる間は待つ」判断を試すテスト用）。pending
    /// （`StartPending`／`StopPending`）でない状態を渡すと checkpoint・
    /// wait hint は 0 として返す。
    pub fn set_pending_progress(&self, scm_state: ScmState, checkpoint: u32, wait_hint: Duration) {
        let mut state = self.state.lock().expect("mock state mutex poisoned");
        let pending = matches!(scm_state, ScmState::StartPending | ScmState::StopPending);
        state.scm_state = scm_state;
        state.checkpoint = if pending { checkpoint } else { 0 };
        state.wait_hint = if pending { wait_hint } else { Duration::ZERO };
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
            checkpoint: state.checkpoint,
            wait_hint: state.wait_hint,
            win32_exit_code: 0,
            service_specific_exit_code: 0,
        })
    }

    fn start(&self) -> Result<TransitionHandle, ServiceManagerError> {
        self.start_with_args(&[])
    }

    fn start_with_args(
        &self,
        args: &[std::ffi::OsString],
    ) -> Result<TransitionHandle, ServiceManagerError> {
        let mut state = self.state.lock().expect("mock state mutex poisoned");
        if let Some(err) = Self::take_injected_error(&mut state) {
            return Err(err);
        }
        if !state.installed {
            return Err(ServiceManagerError::NotFound(state.service_name.clone()));
        }
        if args.is_empty() {
            // 冪等（設計 §4「多重クリック対策」）: 既に Running ならそのまま。
            if state.scm_state == ScmState::Running {
                return Ok(TransitionHandle::new(ScmState::Running));
            }
        } else if state.scm_state != ScmState::Stopped {
            // 引数つきは Stopped のときだけ（引数が渡らない Ok を作らない）。
            return Err(ServiceManagerError::InvalidState(state.scm_state.clone()));
        }
        state.scm_state = ScmState::Running;
        state.pid = Some(MOCK_PID);
        state.checkpoint = 0;
        state.wait_hint = Duration::ZERO;
        state.last_start_args = args.to_vec();
        Ok(TransitionHandle::new(ScmState::Running))
    }

    fn stop(&self) -> Result<TransitionHandle, ServiceManagerError> {
        let mut state = self.state.lock().expect("mock state mutex poisoned");
        if let Some(err) = Self::take_injected_error(&mut state) {
            return Err(err);
        }
        if !state.installed {
            return Err(ServiceManagerError::NotFound(state.service_name.clone()));
        }
        if state.scm_state != ScmState::Stopped {
            state.scm_state = ScmState::Stopped;
            state.pid = None;
            state.checkpoint = 0;
            state.wait_hint = Duration::ZERO;
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
            return Err(ServiceManagerError::NotFound(state.service_name.clone()));
        }
        state.auto_start = enabled;
        Ok(())
    }
}

/// Windows 実 SCM を叩く[`ServiceManager`]実装（T17-0 スコープ）。
///
/// 各製品のサービス登録（`install`/`uninstall`）とは独立した経路 -
/// 登録は [`install_service`]/[`uninstall_service`] が行い、この型を
/// 経由しない（同じ [`ServiceSpec`] から [`WindowsServiceManager::for_spec`]
/// で作れば、登録と同じ名前・表示名・実行ファイル・引数になる）。
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
/// 壊れる。安全のため[`WindowsServiceManager`]は登録（[`install_service`]）と
/// 同じ組み立て方
/// （`executable_path` + `launch_arguments`）で`ServiceInfo`を再構築する -
/// 呼び出し時点で
/// **実際にサービスへ登録されている実行ファイルパスと`executable_path`が
/// 一致している**ことが前提（T17-0 時点の唯一の呼び出し元は
/// console/service ホスト自身であり、これは常に成り立つ。T16-2 が別プロセス
/// から呼ぶ場合は、正しい headless exe パスを構築して渡す責務を呼び出し側が
/// 持つ）。
#[cfg(windows)]
pub struct WindowsServiceManager {
    /// SCM 上のサービス名。
    service_name: &'static str,
    /// `set_auto_start`の`ServiceInfo`再構築で使う表示名。
    display_name: &'static str,
    /// `set_auto_start`がサービス再登録時に使う実行ファイルパス（上記
    /// 制約参照）。
    executable_path: std::path::PathBuf,
    /// `set_auto_start`の`ServiceInfo`再構築で使う起動引数（SCM が
    /// サービス開始時に子プロセスへ渡すものと同じ列を呼び出し側が指定する）。
    launch_arguments: Vec<std::ffi::OsString>,
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
        WindowsServiceManager,
    };

    const SERVICE_TYPE: ServiceType = ServiceType::OWN_PROCESS;

    // `ERROR_SERVICE_DOES_NOT_EXIST`（サービス未登録の判定）は 1 か所に定義した
    // `crate::install` のものを使う（#658。三つに散らばっていた）。
    use crate::ERROR_SERVICE_DOES_NOT_EXIST;
    /// Win32 エラーコード（`ERROR_ACCESS_DENIED`）。
    const ERROR_ACCESS_DENIED: i32 = 5;
    /// Win32 エラーコード（`ERROR_SERVICE_ALREADY_RUNNING`。#623 PR-4）。
    const ERROR_SERVICE_ALREADY_RUNNING: i32 = 1056;
    /// Win32 エラーコード（`ERROR_SERVICE_CANNOT_ACCEPT_CTRL`。#623 PR-4）。
    const ERROR_SERVICE_CANNOT_ACCEPT_CTRL: i32 = 1061;
    /// Win32 エラーコード（`ERROR_SERVICE_SPECIFIC_ERROR`。
    /// `dwServiceSpecificExitCode` を使う印）。
    const ERROR_SERVICE_SPECIFIC_ERROR: u32 = 1066;

    impl WindowsServiceManager {
        /// 任意のサービスを対象にするコンストラクタ（サービス名・表示名・
        /// 起動引数は呼び出し側が渡す）。`executable_path`は`set_auto_start`
        /// での再登録に使う実行ファイルパス（構造体 doc 参照）で、通常は
        /// `std::env::current_exe()`を渡す。製品固有の固定値で組み立てる
        /// ラッパー（banto-hub の`BantoHub`向け等）は製品側 crate が持つ。
        pub fn for_service(
            service_name: &'static str,
            display_name: &'static str,
            executable_path: PathBuf,
            launch_arguments: Vec<OsString>,
        ) -> Self {
            Self {
                service_name,
                display_name,
                executable_path,
                launch_arguments,
            }
        }

        /// 登録と同じ [`crate::ServiceSpec`] から作る（名前・表示名・
        /// 実行ファイル・起動引数を 1 か所で決めるため。#392 B3-1）。
        pub fn for_spec(spec: &crate::ServiceSpec) -> Self {
            Self::for_service(
                spec.name,
                spec.display_name,
                spec.executable.clone(),
                spec.launch_arguments.clone(),
            )
        }
    }

    /// `windows-service` のエラーを [`ServiceManagerError`] に写す。
    /// 未登録と権限不足だけを分け、残りは診断用の文字列。PRESHUTDOWN の
    /// 待ち時間の設定（`shutdown::ensure_preshutdown_timeout`）も同じ写し方を使う。
    pub(crate) fn map_win_err(
        service_name: &str,
        err: windows_service::Error,
    ) -> ServiceManagerError {
        if let windows_service::Error::Winapi(io_err) = &err {
            match io_err.raw_os_error() {
                Some(ERROR_SERVICE_DOES_NOT_EXIST) => {
                    return ServiceManagerError::NotFound(service_name.to_string());
                }
                Some(ERROR_ACCESS_DENIED) => return ServiceManagerError::AccessDenied,
                Some(ERROR_SERVICE_ALREADY_RUNNING) => return ServiceManagerError::AlreadyRunning,
                Some(ERROR_SERVICE_CANNOT_ACCEPT_CTRL) => {
                    return ServiceManagerError::CannotAcceptControl;
                }
                _ => {}
            }
        }
        ServiceManagerError::Other(err.to_string())
    }

    fn open_scm(service_name: &str, access: WinScmAccess) -> Result<WinScm, ServiceManagerError> {
        WinScm::local_computer(None::<&str>, access).map_err(|err| map_win_err(service_name, err))
    }

    /// `open_service`失敗を「未登録」と「その他のエラー」に分ける共通処理。
    /// 未登録は[`ServiceManagerError`]ではなく`Ok(None)`として返し、呼び出し
    /// 側（`query_status`）が`ScmState::NotInstalled`へマップする（設計 §4
    /// の契約「サービス未登録はエラーではない」）。
    fn try_open_service(
        manager: &WinScm,
        service_name: &str,
        access: ServiceAccess,
    ) -> Result<Option<Service>, ServiceManagerError> {
        match manager.open_service(service_name, access) {
            Ok(service) => Ok(Some(service)),
            Err(err) => {
                if let windows_service::Error::Winapi(io_err) = &err {
                    if io_err.raw_os_error() == Some(ERROR_SERVICE_DOES_NOT_EXIST) {
                        return Ok(None);
                    }
                }
                Err(map_win_err(service_name, err))
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

    /// `ServiceStatus::exit_code` を `dwWin32ExitCode` に戻す
    /// （`ERROR_SERVICE_SPECIFIC_ERROR` はサービス固有コードを使う印）。
    fn win32_exit_code(code: &windows_service::service::ServiceExitCode) -> u32 {
        match code {
            windows_service::service::ServiceExitCode::Win32(n) => *n,
            windows_service::service::ServiceExitCode::ServiceSpecific(_) => {
                ERROR_SERVICE_SPECIFIC_ERROR
            }
        }
    }

    /// `dwServiceSpecificExitCode`。`Win32` のときは 0。
    fn specific_exit_code(code: &windows_service::service::ServiceExitCode) -> u32 {
        match code {
            windows_service::service::ServiceExitCode::Win32(_) => 0,
            windows_service::service::ServiceExitCode::ServiceSpecific(n) => *n,
        }
    }

    impl ServiceManager for WindowsServiceManager {
        fn query_status(&self) -> Result<ServiceStatusSummary, ServiceManagerError> {
            let manager = open_scm(self.service_name, WinScmAccess::CONNECT)?;
            let access = ServiceAccess::QUERY_STATUS | ServiceAccess::QUERY_CONFIG;
            let service = match try_open_service(&manager, self.service_name, access)? {
                Some(service) => service,
                None => {
                    return Ok(ServiceStatusSummary {
                        state: ScmState::NotInstalled,
                        auto_start: false,
                        pid: None,
                        checkpoint: 0,
                        wait_hint: std::time::Duration::ZERO,
                        win32_exit_code: 0,
                        service_specific_exit_code: 0,
                    });
                }
            };
            let status = service
                .query_status()
                .map_err(|err| map_win_err(self.service_name, err))?;
            let config = service
                .query_config()
                .map_err(|err| map_win_err(self.service_name, err))?;
            Ok(ServiceStatusSummary {
                state: map_state(status.current_state),
                auto_start: matches!(config.start_type, ServiceStartType::AutoStart),
                pid: status.process_id,
                checkpoint: status.checkpoint,
                wait_hint: status.wait_hint,
                win32_exit_code: win32_exit_code(&status.exit_code),
                service_specific_exit_code: specific_exit_code(&status.exit_code),
            })
        }

        fn start(&self) -> Result<TransitionHandle, ServiceManagerError> {
            self.start_with_args(&[])
        }

        fn start_with_args(
            &self,
            args: &[OsString],
        ) -> Result<TransitionHandle, ServiceManagerError> {
            let manager = open_scm(self.service_name, WinScmAccess::CONNECT)?;
            let access = ServiceAccess::START | ServiceAccess::QUERY_STATUS;
            let service = try_open_service(&manager, self.service_name, access)?
                .ok_or_else(|| ServiceManagerError::NotFound(self.service_name.to_string()))?;
            let status = service
                .query_status()
                .map_err(|err| map_win_err(self.service_name, err))?;
            if args.is_empty() {
                // 冪等（設計 §4「多重クリック対策」）。
                if status.current_state != ServiceState::Running {
                    service
                        .start(args)
                        .map_err(|err| map_win_err(self.service_name, err))?;
                }
            } else if status.current_state != ServiceState::Stopped {
                // 引数つきは Stopped のときだけ（引数が渡らない Ok を作らない）。
                return Err(ServiceManagerError::InvalidState(map_state(
                    status.current_state,
                )));
            } else {
                service
                    .start(args)
                    .map_err(|err| map_win_err(self.service_name, err))?;
            }
            Ok(TransitionHandle::new(ScmState::Running))
        }

        fn stop(&self) -> Result<TransitionHandle, ServiceManagerError> {
            let manager = open_scm(self.service_name, WinScmAccess::CONNECT)?;
            let access = ServiceAccess::STOP | ServiceAccess::QUERY_STATUS;
            let service = try_open_service(&manager, self.service_name, access)?
                .ok_or_else(|| ServiceManagerError::NotFound(self.service_name.to_string()))?;
            let status = service
                .query_status()
                .map_err(|err| map_win_err(self.service_name, err))?;
            if status.current_state != ServiceState::Stopped {
                service
                    .stop()
                    .map_err(|err| map_win_err(self.service_name, err))?;
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
            let manager = open_scm(self.service_name, WinScmAccess::CONNECT)?;
            let access = ServiceAccess::CHANGE_CONFIG;
            let service = try_open_service(&manager, self.service_name, access)?
                .ok_or_else(|| ServiceManagerError::NotFound(self.service_name.to_string()))?;
            let start_type = if enabled {
                ServiceStartType::AutoStart
            } else {
                ServiceStartType::OnDemand
            };
            // [`WindowsServiceManager`]の doc「`set_auto_start`の制約」参照 -
            // 製品側の`install`と同じ組み立て方で再構築する。
            let service_info = ServiceInfo {
                name: OsString::from(self.service_name),
                display_name: OsString::from(self.display_name),
                service_type: SERVICE_TYPE,
                start_type,
                error_control: ServiceErrorControl::Normal,
                executable_path: self.executable_path.clone(),
                launch_arguments: self.launch_arguments.clone(),
                dependencies: vec![],
                account_name: None,
                account_password: None,
            };
            service
                .change_config(&service_info)
                .map_err(|err| map_win_err(self.service_name, err))?;
            if enabled {
                // 自動起動を明示的に有効化する経路では、遅延自動開始を併せて
                // 有効にする（banto-hub の`install`の AutoStart 既定と同じ
                // 判断）。AutoStart=遅延あり、OnDemand=該当なし、の対応関係
                // （OnDemand には遅延自動開始の概念が無い）。
                service
                    .set_delayed_auto_start(true)
                    .map_err(|err| map_win_err(self.service_name, err))?;
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
    fn not_found_carries_the_configured_service_name() {
        let manager = MockServiceManager::not_installed().with_service_name("MySvc");
        assert!(matches!(
            manager.start(),
            Err(ServiceManagerError::NotFound(name)) if name == "MySvc"
        ));
        let manager = MockServiceManager::not_installed();
        assert!(matches!(
            manager.stop(),
            Err(ServiceManagerError::NotFound(name)) if name == "mock-service"
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

    /// 文言は製品名・「管理者」を含まない中立な形（B2-4a）。banto-hub の
    /// 従来の文言は banto-hub-core の`describe_service_error`が持つ。
    #[test]
    fn error_display_is_product_neutral() {
        let all = [
            ServiceManagerError::NotFound("ChronoGazer".to_string()),
            ServiceManagerError::AccessDenied,
            ServiceManagerError::Timeout(Duration::from_secs(3)),
            ServiceManagerError::Other("os error 5".to_string()),
            ServiceManagerError::AlreadyRunning,
            ServiceManagerError::CannotAcceptControl,
        ];
        for err in &all {
            let text = err.to_string();
            assert!(!text.contains("banto-hub"), "{text}");
            assert!(!text.contains("管理者"), "{text}");
        }
        assert_eq!(
            ServiceManagerError::AccessDenied.to_string(),
            "サービス操作の権限がありません"
        );
        assert_eq!(
            ServiceManagerError::NotFound("ChronoGazer".to_string()).to_string(),
            "サービス 'ChronoGazer' が見つかりません（未登録の可能性）"
        );
    }

    // `WindowsServiceManager::for_service`のパラメータ化を、非破壊な
    // 読み取り専用操作（`query_status`）だけで実機 Windows 上で検証する。
    // インストール・起動・停止は一切行わない（本 PR のスコープ外・
    // このワークスペースでは禁止されている操作）。
    #[cfg(windows)]
    mod windows_for_service_tests {
        use crate::{ScmState, ServiceManager, WindowsServiceManager};

        /// 実在しないサービス名を渡しても、渡した名前でSCMへ問い合わせて
        /// `NotInstalled`が返ること（特定のサービス名に固定されていない
        /// ことの確認）。
        #[test]
        fn for_service_queries_the_given_service_name_not_the_bantohub_default() {
            let manager = WindowsServiceManager::for_service(
                "BantoHubDoesNotExist12345",
                "存在しないテスト用サービス",
                std::path::PathBuf::new(),
                vec![],
            );
            let status = manager
                .query_status()
                .expect("query_status against SCM must succeed (read-only, no privilege needed)");
            assert_eq!(status.state, ScmState::NotInstalled);
            assert!(!status.auto_start);
            assert_eq!(status.pid, None);
            assert_eq!(status.checkpoint, 0);
            assert_eq!(status.wait_hint, std::time::Duration::ZERO);
        }

        /// 引数つきの開始も、未登録なら（権限に関わらず）`NotFound`
        /// （実サービスは開始しない）。
        #[test]
        fn start_with_args_on_an_unregistered_service_is_not_found() {
            let manager = WindowsServiceManager::for_service(
                "BantoHubDoesNotExist12345",
                "存在しないテスト用サービス",
                std::path::PathBuf::new(),
                vec![],
            );
            let err = manager
                .start_with_args(&["relax-only".into()])
                .expect_err("未登録は開始できない");
            assert!(
                matches!(err, crate::ServiceManagerError::NotFound(_)),
                "{err:?}"
            );
        }
    }

    fn summary(state: ScmState, checkpoint: u32) -> ServiceStatusSummary {
        ServiceStatusSummary {
            state,
            auto_start: false,
            pid: None,
            checkpoint,
            wait_hint: Duration::from_secs(10),
            win32_exit_code: 0,
            service_specific_exit_code: 0,
        }
    }

    #[test]
    fn mock_start_with_args_records_the_arguments_and_start_clears_them() {
        let mock = MockServiceManager::new();
        mock.start_with_args(&["relax-only".into()]).unwrap();
        assert_eq!(mock.current_state(), ScmState::Running);
        assert_eq!(
            mock.last_start_args(),
            vec![std::ffi::OsString::from("relax-only")]
        );
        // Running では引数つきの開始は断る（引数が渡らない Ok を作らない）。
        assert!(matches!(
            mock.start_with_args(&["other".into()]),
            Err(ServiceManagerError::InvalidState(ScmState::Running))
        ));
        assert_eq!(
            mock.last_start_args(),
            vec![std::ffi::OsString::from("relax-only")]
        );
        // 引数が空なら `start` と同じ冪等（Running でも Ok で、引数は変わらない）。
        mock.start_with_args(&[]).unwrap();
        assert_eq!(
            mock.last_start_args(),
            vec![std::ffi::OsString::from("relax-only")]
        );
        // pending でも断る。
        mock.set_pending_progress(ScmState::StartPending, 1, Duration::from_secs(1));
        assert!(matches!(
            mock.start_with_args(&["other".into()]),
            Err(ServiceManagerError::InvalidState(ScmState::StartPending))
        ));
        mock.stop().unwrap();
        mock.start().unwrap();
        assert!(mock.last_start_args().is_empty());
        let missing = MockServiceManager::not_installed();
        assert!(matches!(
            missing.start_with_args(&["x".into()]),
            Err(ServiceManagerError::NotFound(_))
        ));
    }

    #[test]
    fn mock_query_carries_checkpoint_and_wait_hint_only_while_pending() {
        let mock = MockServiceManager::new();
        let idle = mock.query_status().unwrap();
        assert_eq!((idle.checkpoint, idle.wait_hint), (0, Duration::ZERO));
        mock.set_pending_progress(ScmState::StartPending, 3, Duration::from_secs(10));
        let pending = mock.query_status().unwrap();
        assert_eq!(pending.state, ScmState::StartPending);
        assert_eq!(
            (pending.checkpoint, pending.wait_hint),
            (3, Duration::from_secs(10))
        );
        mock.start().unwrap();
        let running = mock.query_status().unwrap();
        assert_eq!((running.checkpoint, running.wait_hint), (0, Duration::ZERO));
        // pending でない状態に checkpoint・wait hint を渡しても 0 で返る。
        mock.set_pending_progress(ScmState::Running, 3, Duration::from_secs(10));
        let forced = mock.query_status().unwrap();
        assert_eq!((forced.checkpoint, forced.wait_hint), (0, Duration::ZERO));
        // 終了コードは正常（0, 0）。
        assert_eq!(
            (forced.win32_exit_code, forced.service_specific_exit_code),
            (0, 0)
        );
    }

    /// (前回, 今回) -> 進んでいるか、の表。
    #[test]
    fn pending_progress_table() {
        use ScmState::*;
        let table: [(
            Option<ServiceStatusSummary>,
            ServiceStatusSummary,
            bool,
            &str,
        ); 9] = [
            (
                None,
                summary(StartPending, 1),
                true,
                "最初の観測の pending は進んでいる扱い",
            ),
            (
                Some(summary(StartPending, 1)),
                summary(StartPending, 2),
                true,
                "増えた",
            ),
            (
                Some(summary(StartPending, 2)),
                summary(StartPending, 2),
                false,
                "止まった",
            ),
            (
                Some(summary(StartPending, 3)),
                summary(StartPending, 2),
                false,
                "戻った",
            ),
            (
                Some(summary(Stopped, 0)),
                summary(StartPending, 1),
                true,
                "新しい pending",
            ),
            (
                Some(summary(StartPending, 5)),
                summary(StopPending, 1),
                true,
                "別の pending へ",
            ),
            (
                Some(summary(StopPending, 4)),
                summary(StopPending, 4),
                false,
                "停止も同じ",
            ),
            (
                Some(summary(StartPending, 4)),
                summary(Running, 0),
                false,
                "pending でない",
            ),
            (None, summary(Stopped, 0), false, "停止済みは待たない"),
        ];
        for (previous, current, expected, why) in table {
            assert_eq!(
                pending_progress(previous.as_ref(), &current),
                expected,
                "{why}"
            );
        }
    }
}
