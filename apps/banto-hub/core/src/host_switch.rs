//! T17-3（docs/banto-hub-t17-design.md §3「T17-3」、
//! docs/banto-hub-desktop-plan.md §9.9「タスクトレイと停止時 fallback」・
//! §16.3「desktop⇔service 切替の中間状態を追加」）: Desktop↔Service 切替
//! トランザクションの状態機械。
//!
//! 実行ホストは定常状態としては[`HostKind`]（`Offline`/`Desktop`/`Service`）
//! の3値だが、切替は2プロセス（デスクトップシェルと Windows サービス）と
//! SCM を跨ぐため、`crate::controller::CollectionController`のような単一
//! プロセス内の直列化では二重接続を防げない（desktop-plan §16.3
//! 「hub 内 controller の直列化では守れない」）。このモジュールは切替の
//! 各段階を[`SwitchPhase`]という型で表現し、[`HostSwitchEngine::step`]で
//! 1ステップずつ進める - 進行状態の**所有者はシェル（ネイティブ側）**であり
//! （desktop-plan 同節）、このモジュール自身はタイマー・スレッドを持たず、
//! シェルが`SwitchCommand::Poll`を繰り返し呼ぶことで前進する。
//!
//! ## 二重接続を起こさないための設計（実装指示の不変条件そのもの）
//!
//! 1. [`crate::service_manager::ServiceManager::start`]を呼ぶ前に、Desktop が
//!    停止し（[`DesktopHostControl::is_stopped`]）、mutex 相当の解放が
//!    確認できていること（モック上は同じ`is_stopped`が両方を表す）。
//! 2. Desktop の起動を許可する前に、Service が`Stopped`
//!    （[`crate::service_manager::ServiceManager::query_status`]）かつ旧
//!    health が消失していること（[`crate::hub_health::HubHealthProbe`]が
//!    [`HealthOutcome::Unreachable`]を返す）。
//! 3. どの段階で失敗しても「もう一方を起動したまま戻る」ことはない - 失敗
//!    到達状態（[`SwitchPhase::Faulted`]）の`reached`ホストは各段階ごとに
//!    安全側（既に確認済みの側）を選ぶ（後述の表参照）。
//! 4. `Idle`/`Faulted`（終端）以外の間は新しい`SwitchToService`/
//!    `SwitchToDesktop`を受け付けず[`StepOutcome::TransitionInProgress`]を
//!    返す（desktop-plan §4.3「遷移中は新しい開始・停止要求を重ねない」の
//!    Desktop↔Service 版）。
//! 5. [`HealthOutcome::WrongProfileOrVersion`]/[`HealthOutcome::MutexOwnerUnknown`]/
//!    [`HealthOutcome::PortConflict`]の間は`Service`開始成功と見なさない -
//!    [`HostSwitchState::last_health`]に残し、fallback UI（T16-2）が
//!    「開始ボタンを隠す」判断材料に使える。
//!
//! ## 段階（[`SwitchPhase`]）と失敗到達（[`FaultStage`]→到達 [`HostKind`]）
//!
//! Desktop → Service:
//!
//! | 段階 | 失敗到達 host |
//! | --- | --- |
//! | `DesktopStopping`（`request_stop`失敗） | `Desktop`（未着手のまま） |
//! | `AwaitingDesktopRelease`（timeout） | `Desktop`（未確認のまま） |
//! | `ServiceStarting`（`start`失敗/timeout） | `Offline`（Desktop 解放済み・Service 未確認） |
//! | `AwaitingServiceHealth`（timeout） | SCM が`Running`なら`Service`（Unhealthy）、それ以外は`Offline` |
//!
//! Service → Desktop:
//!
//! | 段階 | 失敗到達 host |
//! | --- | --- |
//! | `ServiceStopping`（`stop`失敗） | `Service`（未着手のまま） |
//! | `AwaitingServiceRelease`（timeout） | `Service`（未確認のまま） |
//! | `DesktopStarting`（許可 timeout） | `Offline`（Service 解放済み・Desktop 未起動） |
//!
//! いずれの失敗到達でも実機収集を自動再開しない
//! （[`HostSwitchEngine::step`]は`Faulted`到達後、呼び出し側が明示的に
//! 新しい`SwitchTo*`を送るまで何もしない）。
//!
//! ## T17-2（UAC/Operators）との関係
//!
//! `BantoHub Operators`メンバーシップ確認（T17-2）は本スライスではスタブ -
//! [`HostSwitchEngine`]は`can_operate_service: bool`を構築時に受け取り、
//! `false`の間は[`crate::service_manager::ServiceManager::start`]/`stop`を
//! 一切呼ばず[`SwitchError::PermissionDenied`]を返す。Desktop 自身の
//! 起動・停止（[`DesktopHostControl`]）はこの権限確認の対象外
//! （Windows サービス操作ではないため）。

use std::time::{Duration, Instant};

use thiserror::Error;

use crate::hub_health::{HealthOutcome, HubHealthProbe};
use crate::service_manager::{
    describe_service_error, service_exit_with, PendingWatch, ScmState, ServiceEnd, ServiceManager,
    ServiceManagerError, ServiceStatusSummary, SwitchTimings, WatchStep, DEFAULT_OVERALL_LIMIT,
    MAX_STOP_RETRIES,
};
// 緩めるだけの開始（`relax-only`）の口は共有 crate の型をそのまま使う（#661 661-4）。
pub use crate::service_manager::{RealClock, RelaxOnlyOutcome, SwitchClock};

/// サービスの終了コード 4: 運用モードにできなかった（何も変えずに断った・締めかけて緩め直した）。
/// サービス側（`bin/banto_hub/win_service.rs`）の値と同じ。
pub const SERVICE_EXIT_CANNOT_OPERATE: u32 = 4;

/// サービスの終了コード 5: 締めたまま止まった（緩められなかった）。サービス側の値と同じ。
pub const SERVICE_EXIT_LEFT_TIGHT: u32 = 5;

/// 緩めるだけの開始でサービスに渡す引数（サービス側が受ける値）。
pub const RELAX_ONLY_ARG: &str = "relax-only";

/// 本番の待ち時間（段 15 秒・照会 250ms は `src-tauri` の `HOST_SWITCH_PHASE_TIMEOUT`・
/// `HOST_SWITCH_POLL_INTERVAL` と同じ。全体 30 分は ChronoGazer と同じ）。checkpoint が進む間は
/// 段の期限を延ばし、全体の上限で止める（[`HostSwitchEngine`]・[`relax_only_and_wait`]）。
pub const SWITCH_TIMINGS: SwitchTimings = SwitchTimings {
    poll: Duration::from_millis(250),
    phase: Duration::from_secs(15),
    overall: DEFAULT_OVERALL_LIMIT,
};

/// サービスが Running を経ずに（または停止の結果として）自分で止まった理由のうち、画面が
/// 「運用モードにできない」「締めたまま残った」と案内を分けるもの（終了コード 4・5）。
/// [`HostSwitchState::service_fault`]に載る。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceFault {
    /// 終了コード 4: 運用モードにできなかった。モードは設定モードのまま。
    CannotOperate,
    /// 終了コード 5: 締めたまま残った。モードは運用モードのまま（緩めるには `relax-only`）。
    LeftTight,
}

impl std::fmt::Display for ServiceFault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ServiceFault::CannotOperate => {
                write!(f, "運用モードにできませんでした（終了コード 4）")
            }
            ServiceFault::LeftTight => write!(f, "運用モードのまま残りました（終了コード 5）"),
        }
    }
}

/// 止まったサービスの終了コードを [`ServiceFault`] に読む（純関数。4・5 以外は`None`）。
pub fn service_fault(summary: &ServiceStatusSummary) -> Option<ServiceFault> {
    match service_exit_with(
        summary,
        SERVICE_EXIT_CANNOT_OPERATE,
        SERVICE_EXIT_LEFT_TIGHT,
    ) {
        ServiceEnd::CannotOperate => Some(ServiceFault::CannotOperate),
        ServiceEnd::LeftOperation => Some(ServiceFault::LeftTight),
        ServiceEnd::Clean | ServiceEnd::Other { .. } => None,
    }
}

/// 緩めるだけの開始（`relax-only`）: `start_with_args(["relax-only"])`（Stopped のときだけ。
/// Operators なら UAC なし）→ `Ok` の**後に**照会を始め、最初の Stopped を完了と見る。結果は
/// [`RelaxOnlyOutcome::Done`]の終了の理由（`Clean` = 0 緩めた、`LeftOperation` = 5 締めたまま）。
/// 画面への配線は 661-5。
pub fn relax_only_and_wait(
    manager: &impl ServiceManager,
    clock: &impl SwitchClock,
    timings: SwitchTimings,
) -> RelaxOnlyOutcome {
    banto_service_control::relax_only_and_wait(
        manager,
        clock,
        timings,
        RELAX_ONLY_ARG,
        SERVICE_EXIT_CANNOT_OPERATE,
        SERVICE_EXIT_LEFT_TIGHT,
    )
}

/// 定常状態として観測される実行ホスト（desktop-plan §16.3
/// 「実行ホストは offline/desktop/service の3値」）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostKind {
    /// Desktop も Service も稼働していない。
    Offline,
    /// デスクトップシェル（`apps/banto-hub/src-tauri`）が Hub を保有している。
    Desktop,
    /// Windows サービス（`BantoHub`）が Hub を保有している。SCM が
    /// `Running`であることを意味するだけで、health が健全とは限らない
    /// （[`HostSwitchState::last_health`]参照 - `Faulted`到達後の
    /// 「Service だが Unhealthy」を表すのに使う）。
    Service,
}

impl std::fmt::Display for HostKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HostKind::Offline => write!(f, "Offline"),
            HostKind::Desktop => write!(f, "Desktop"),
            HostKind::Service => write!(f, "Service"),
        }
    }
}

/// 失敗到達時にどの段階で止まったかを表す（[`SwitchPhase::Faulted`]の
/// `stage`）。[`SwitchPhase`]の各進行中バリアントと1対1で対応する。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FaultStage {
    DesktopStopping,
    AwaitingDesktopRelease,
    ServiceStarting,
    AwaitingServiceHealth,
    ServiceStopping,
    AwaitingServiceRelease,
    DesktopStarting,
}

impl std::fmt::Display for FaultStage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FaultStage::DesktopStopping => write!(f, "DesktopStopping"),
            FaultStage::AwaitingDesktopRelease => write!(f, "AwaitingDesktopRelease"),
            FaultStage::ServiceStarting => write!(f, "ServiceStarting"),
            FaultStage::AwaitingServiceHealth => write!(f, "AwaitingServiceHealth"),
            FaultStage::ServiceStopping => write!(f, "ServiceStopping"),
            FaultStage::AwaitingServiceRelease => write!(f, "AwaitingServiceRelease"),
            FaultStage::DesktopStarting => write!(f, "DesktopStarting"),
        }
    }
}

/// 切替の進行段階（`Idle`は「進行中でない」を表す - 定常状態そのものは
/// [`HostSwitchState::current`]が持つ）。
///
/// `DesktopStopping`は「停止要求を発行した瞬間」を表す -
/// この実装では要求自体が同期 API（[`DesktopHostControl::request_stop`]）なので、
/// 成功時は同じ`step`呼び出し内で即座に次段（`AwaitingDesktopRelease`）へ進み、
/// 外部からは定常観測されない。要求自体が失敗した場合にのみ
/// [`FaultStage::DesktopStopping`]として`Faulted`に残る。
///
/// `ServiceStopping`は、サービスの STOP が 1061／1052 で断られた間（権限の設定の途中 =
/// 「切り替え中」。#661 661-4）だけ観測される待ちで、STOP が受け付けられれば同じ`step`内で
/// `AwaitingServiceRelease`へ進む。
///
/// SCM の pending を待つ段（`ServiceStarting`・`ServiceStopping`・`AwaitingServiceRelease`）は、
/// checkpoint が進む間は期限を延ばし（[`PendingWatch`]）、止まったまま段の待ち時間を過ぎるか
/// 全体の上限（[`DEFAULT_OVERALL_LIMIT`]）を超えたら失敗にする。`ServiceStarting`で Running を
/// 経ずに Stopped になったときは終了コードで分け、4 は[`ServiceFault::CannotOperate`]、
/// 5 は[`ServiceFault::LeftTight`]を[`HostSwitchState::service_fault`]に載せて`Faulted`にする。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SwitchPhase {
    Idle,
    /// Desktop Collector/Hub 停止要求中（このモジュール doc 参照）。
    DesktopStopping,
    /// health 消失・mutex 解放待ち（[`DesktopHostControl::is_stopped`]を
    /// ポーリング）。
    AwaitingDesktopRelease,
    /// SCM `start()`発行〜settled（[`ServiceManager::query_status`]で
    /// `Running`になるまでポーリング）。
    ServiceStarting,
    /// probe Healthy 待ち（[`HubHealthProbe::probe`]をポーリング）。
    AwaitingServiceHealth,
    /// SCM `stop()`が 1061／1052 で断られ、切り替え中（START_PENDING／STOP_PENDING）が
    /// 終わるのを待つ。Running に戻ったら STOP を出し直す（[`MAX_STOP_RETRIES`]回まで）。
    ServiceStopping,
    /// `Stopped` + 旧 health 消失待ち。
    AwaitingServiceRelease,
    /// Desktop 側の起動許可待ち（実際の`HubRuntime::start`はシェル側が呼ぶ -
    /// このモジュールは「開始してよい」許可を返すだけ、モジュール doc 参照）。
    DesktopStarting,
    /// 失敗到達（安全側）。`stage`は失敗した段階、`reason`は診断用文言。
    /// 到達後は明示的な`SwitchCommand::SwitchToService`/`SwitchToDesktop`が
    /// 来るまで何もしない（実機収集を自動再開しない）。
    Faulted {
        stage: FaultStage,
        reason: String,
    },
}

/// [`HostSwitchEngine`]が保持する状態そのもの。シェル側はこれを読んで UI
/// （トレイ・fallback 画面）を更新する。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostSwitchState {
    /// 定常として観測される実行ホスト（進行中も、直前に確認できた安全な
    /// 値を保つ - 例: Desktop 解放を確認した瞬間に`Offline`へ進める）。
    pub current: HostKind,
    pub phase: SwitchPhase,
    /// 直前の[`HubHealthProbe::probe`]結果（`AwaitingServiceHealth`/
    /// `AwaitingServiceRelease`で更新）。fallback UI が「別 profile/version」
    /// 「mutex 所有者不明」等の文言を出す材料（不変条件5、モジュール doc）。
    pub last_health: Option<HealthOutcome>,
    /// サービスが自分で止まった理由が終了コード 4・5 だったとき（[`ServiceFault`]）。
    /// 新しい`SwitchTo*`の受け付けで消える。`Faulted`の`reason`にも文言は入る。
    pub service_fault: Option<ServiceFault>,
}

/// [`HostSwitchEngine::step`]へ渡すコマンド。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SwitchCommand {
    SwitchToService,
    SwitchToDesktop,
    /// 進行中の切替を安全側で打ち切る（`Idle`/`Faulted`時は no-op）。
    Cancel,
    /// タイムアウト付き待ちを1ステップ進める - シェルが自前のタイマーで
    /// 繰り返し呼ぶことを想定する。
    Poll,
}

/// [`HostSwitchEngine::step`]の結果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StepOutcome {
    /// すでに目的のホストだった等、何もしなかった（冪等 - desktop-plan
    /// §4.3「多重クリック対策」の Desktop↔Service 版）。
    NoOp,
    /// 別の切替が進行中のため新しい`SwitchTo*`を無視した（不変条件4）。
    TransitionInProgress,
    /// 現在の段階のまま、まだ完了条件を満たさない（次の`Poll`を待つ）。
    Waiting,
    /// 次の段階へ進んだ。
    Progressed,
    /// 切替が成功して完了した。
    Completed(HostKind),
    /// 失敗到達（安全側）。
    Faulted {
        reached: HostKind,
        stage: FaultStage,
        reason: String,
    },
}

/// [`HostSwitchEngine::step`]自体が受け付けなかったことを表すエラー -
/// [`StepOutcome::Faulted`]（副作用を試みたが失敗した）とは区別する。
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum SwitchError {
    /// `can_operate_service`が`false`（T17-2 UAC/Operators 確認のスタブ）。
    /// `ServiceManager::start`/`stop`を一切呼ばずに返す。
    #[error("banto-hub: サービス操作の権限がありません（BantoHub Operators または管理者権限が必要です）")]
    PermissionDenied,
}

/// [`DesktopHostControl::request_stop`]の失敗。
#[derive(Debug, Clone, Error)]
pub enum DesktopHostError {
    #[error("banto-hub: Desktop Hub の停止要求に失敗しました: {0}")]
    Other(String),
}

/// Desktop 側（デスクトップシェルが保有する`HubRuntime`）の停止要求・起動
/// 許可判定を分離するコールバック trait。
///
/// [`HostSwitchEngine`]（`banto_hub_core`側のロジック層）はこの trait
/// だけを消費し、`HubRuntime::start`/`shutdown`を直接呼ばない - 実際の
/// Desktop Hub 起動はシェル（`apps/banto-hub/src-tauri`）側の責務
/// （モジュール doc・実装指示「コアが HubRuntime を直接触らない」）。
pub trait DesktopHostControl {
    /// Desktop Hub/Collector の停止を要求する - 副作用は「要求した」ことの
    /// 記録のみで、実際の停止完了は[`Self::is_stopped`]で確認する。
    fn request_stop(&mut self) -> Result<(), DesktopHostError>;

    /// Desktop Hub が完全に停止し、profile mutex（T17-1）を解放済みか -
    /// 両方を満たしたときだけ`true`を返す実装にすること（不変条件1、
    /// モジュール doc）。
    fn is_stopped(&self) -> bool;

    /// Desktop 側の起動を engine が許可してよいか。mutex/SCM 条件自体は
    /// [`HostSwitchEngine`]が[`ServiceManager`]/[`HubHealthProbe`]で確認
    /// 済みの状態でのみ呼ばれる - この戻り値は追加の Windows 操作権限等の
    /// 確認用フックで、既定実装は常に許可する。
    fn request_start_allowed(&self) -> bool {
        true
    }
}

/// [`DesktopHostControl`]のインメモリモック実装 - 単体テスト用
/// （このファイル末尾の`tests`モジュール）。
pub struct MockDesktopHostControl {
    stop_requested: bool,
    stopped: bool,
    mutex_released: bool,
    start_allowed: bool,
    next_stop_error: Option<DesktopHostError>,
}

impl MockDesktopHostControl {
    /// Desktop が稼働中（停止未確認・mutex 保持中）から始まるモック -
    /// Desktop→Service 切替のテストの既定初期状態。
    pub fn new() -> Self {
        Self {
            stop_requested: false,
            stopped: false,
            mutex_released: false,
            start_allowed: true,
            next_stop_error: None,
        }
    }

    pub fn set_stopped(&mut self, value: bool) {
        self.stopped = value;
    }

    pub fn set_mutex_released(&mut self, value: bool) {
        self.mutex_released = value;
    }

    /// [`Self::set_stopped`]+[`Self::set_mutex_released`]の同時設定 -
    /// 「停止も mutex 解放も完了した」を一度に表現するテスト用ヘルパー。
    pub fn set_released(&mut self, value: bool) {
        self.stopped = value;
        self.mutex_released = value;
    }

    pub fn set_start_allowed(&mut self, value: bool) {
        self.start_allowed = value;
    }

    /// 次の1回の`request_stop`でだけ返すエラーを設定する。
    pub fn inject_stop_error(&mut self, err: DesktopHostError) {
        self.next_stop_error = Some(err);
    }

    pub fn stop_was_requested(&self) -> bool {
        self.stop_requested
    }
}

impl Default for MockDesktopHostControl {
    fn default() -> Self {
        Self::new()
    }
}

impl DesktopHostControl for MockDesktopHostControl {
    fn request_stop(&mut self) -> Result<(), DesktopHostError> {
        if let Some(err) = self.next_stop_error.take() {
            return Err(err);
        }
        self.stop_requested = true;
        Ok(())
    }

    fn is_stopped(&self) -> bool {
        self.stopped && self.mutex_released
    }

    fn request_start_allowed(&self) -> bool {
        self.start_allowed
    }
}

/// [`HostSwitchEngine::new`]へ渡す構築パラメータ - 引数個数を抑えるための
/// 薄い構造体（フィールド自体に固有のロジックは持たない）。
pub struct HostSwitchConfig {
    /// [`HubHealthProbe::probe`]へ渡す期待 profile-id
    /// （`crate::profile_paths::ProfilePaths::profile_id`）。
    pub expected_profile: String,
    /// [`HubHealthProbe::probe`]へ渡す期待ポート。
    pub expected_port: u16,
    /// `BantoHub Operators`メンバーシップ確認（T17-2）のスタブ - `false`の
    /// 間は[`ServiceManager::start`]/`stop`を呼ばない（モジュール doc）。
    pub can_operate_service: bool,
    /// 構築時点の実行ホスト（通常は起動時に一度観測した値）。
    pub initial_host: HostKind,
    /// `AwaitingDesktopRelease`/`ServiceStarting`/`AwaitingServiceHealth`/
    /// `AwaitingServiceRelease`/`DesktopStarting`各段階の待ち上限。段階ごと
    /// に別の値が必要になったら段階別フィールドへ分ける（今回は実装指示の
    /// スコープ上、単一値で十分）。
    pub phase_timeout: Duration,
}

/// T17-3 の中核 - Desktop↔Service 切替を1ステップずつ進める状態機械。
///
/// 進行状態の所有者はシェル（このモジュール doc）- `step`はブロッキング
/// せず即座に返り、待ちが必要な段階は`SwitchCommand::Poll`を繰り返し送る
/// ことで前進する（内部でスレッドを起動しない）。
pub struct HostSwitchEngine<M, P, D>
where
    M: ServiceManager,
    P: HubHealthProbe,
    D: DesktopHostControl,
{
    service_manager: M,
    probe: P,
    desktop: D,
    expected_profile: String,
    expected_port: u16,
    can_operate_service: bool,
    phase_timeout: Duration,
    /// 現在の待ち段階の期限（`Idle`/`Faulted`では`None`）。
    phase_deadline: Option<Instant>,
    state: HostSwitchState,
    /// 段の待ちの全体の上限（checkpoint が進み続けてもこれで止める。既定 30 分）。
    overall_limit: Duration,
    /// SCM の pending を待つ段（`ServiceStarting`・`ServiceStopping`・`AwaitingServiceRelease`）の
    /// 待ち。checkpoint が進む間は期限を延ばす（[`PendingWatch`]）。
    watch: Option<PendingWatch>,
    /// 1061／1052 で断られた STOP を出し直した回数（[`MAX_STOP_RETRIES`]まで）。
    stop_retries: u32,
    /// テストが時計を進めるための差（本番は常に 0）。
    clock_offset: Duration,
}

impl<M, P, D> HostSwitchEngine<M, P, D>
where
    M: ServiceManager,
    P: HubHealthProbe,
    D: DesktopHostControl,
{
    pub fn new(service_manager: M, probe: P, desktop: D, config: HostSwitchConfig) -> Self {
        Self {
            service_manager,
            probe,
            desktop,
            expected_profile: config.expected_profile,
            expected_port: config.expected_port,
            can_operate_service: config.can_operate_service,
            phase_timeout: config.phase_timeout,
            phase_deadline: None,
            state: HostSwitchState {
                current: config.initial_host,
                phase: SwitchPhase::Idle,
                last_health: None,
                service_fault: None,
            },
            overall_limit: DEFAULT_OVERALL_LIMIT,
            watch: None,
            stop_retries: 0,
            clock_offset: Duration::ZERO,
        }
    }

    /// 全体の上限を変える（既定 [`DEFAULT_OVERALL_LIMIT`]）。
    pub fn set_overall_limit(&mut self, value: Duration) {
        self.overall_limit = value;
    }

    pub fn state(&self) -> &HostSwitchState {
        &self.state
    }

    /// T17-2（UAC/Operators）の確認結果が変わったときにシェル側が反映する
    /// ためのセッタ - engine 再構築を避けるためのフック。
    pub fn set_can_operate_service(&mut self, value: bool) {
        self.can_operate_service = value;
    }

    /// 1ステップ進める。副作用は[`ServiceManager`]/[`HubHealthProbe`]/
    /// [`DesktopHostControl`]のみ（モジュール doc）。
    pub fn step(&mut self, cmd: SwitchCommand) -> Result<StepOutcome, SwitchError> {
        match cmd {
            SwitchCommand::SwitchToService => self.handle_switch_to_service(),
            SwitchCommand::SwitchToDesktop => self.handle_switch_to_desktop(),
            SwitchCommand::Cancel => Ok(self.handle_cancel()),
            SwitchCommand::Poll => Ok(self.handle_poll()),
        }
    }

    /// `Idle`（未進行）または`Faulted`（終端の安全な定常）のときだけ新しい
    /// `SwitchTo*`を受け付ける（不変条件4 - `Faulted`は「進行中」ではない
    /// ので再試行を妨げない）。
    fn is_idle_or_terminal(&self) -> bool {
        matches!(
            self.state.phase,
            SwitchPhase::Idle | SwitchPhase::Faulted { .. }
        )
    }

    fn now(&self) -> Instant {
        Instant::now() + self.clock_offset
    }

    fn enter_phase(&mut self, phase: SwitchPhase) {
        let now = self.now();
        self.phase_deadline = Some(now + self.phase_timeout);
        let target = match phase {
            SwitchPhase::ServiceStarting => Some(ScmState::Running),
            SwitchPhase::ServiceStopping | SwitchPhase::AwaitingServiceRelease => {
                Some(ScmState::Stopped)
            }
            _ => None,
        };
        self.watch = target.map(|target| {
            PendingWatch::new(
                target,
                now,
                SwitchTimings {
                    poll: Duration::ZERO,
                    phase: self.phase_timeout,
                    overall: self.overall_limit,
                },
            )
        });
        self.state.phase = phase;
    }

    fn timed_out(&self) -> bool {
        match self.phase_deadline {
            Some(deadline) => self.now() >= deadline,
            None => false,
        }
    }

    /// 1 回の観測が`Stalled`／`OverallTimeout`だったときの失敗の文言。
    fn watch_expiry_reason(&self, step: &WatchStep) -> String {
        match step {
            WatchStep::OverallTimeout => {
                format!("全体の上限 {:?} を超えました", self.overall_limit)
            }
            _ => format!(
                "checkpoint が進まないまま {:?} を過ぎました（サービスが固まった可能性）",
                self.phase_timeout
            ),
        }
    }

    fn timeout_reason(&self) -> String {
        format!("{:?} 以内に完了しませんでした", self.phase_timeout)
    }

    /// 失敗到達へ遷移する - `reached`は不変条件3（二重接続禁止）を満たす
    /// よう呼び出し側が段階ごとに選ぶ。
    fn fault(&mut self, stage: FaultStage, reached: HostKind, reason: String) -> StepOutcome {
        self.state.current = reached;
        self.phase_deadline = None;
        self.watch = None;
        self.state.phase = SwitchPhase::Faulted {
            stage,
            reason: reason.clone(),
        };
        StepOutcome::Faulted {
            reached,
            stage,
            reason,
        }
    }

    /// 現在の進行段階に応じた安全な`reached`を選んで`Faulted`へ遷移する -
    /// タイムアウト経路と[`SwitchCommand::Cancel`]の両方から共有する
    /// （どちらも「これ以上待たずに安全側の定常へ落とす」という同じ操作）。
    fn fault_for_current_phase(&mut self, reason: String) -> StepOutcome {
        match self.state.phase.clone() {
            SwitchPhase::Idle | SwitchPhase::Faulted { .. } => StepOutcome::NoOp,
            SwitchPhase::DesktopStopping => {
                self.fault(FaultStage::DesktopStopping, HostKind::Desktop, reason)
            }
            SwitchPhase::AwaitingDesktopRelease => self.fault(
                FaultStage::AwaitingDesktopRelease,
                HostKind::Desktop,
                reason,
            ),
            SwitchPhase::ServiceStarting => {
                self.fault(FaultStage::ServiceStarting, HostKind::Offline, reason)
            }
            SwitchPhase::AwaitingServiceHealth => {
                // SCM が実際に Running のままなら「Service だが Unhealthy」
                // （設計 §「失敗後の定常は原則 Offline...サービスが Running
                // のまま応答なしなら Service だが Unhealthy」）。
                let reached = match self.service_manager.query_status() {
                    Ok(status) if status.state == ScmState::Running => HostKind::Service,
                    _ => HostKind::Offline,
                };
                self.fault(FaultStage::AwaitingServiceHealth, reached, reason)
            }
            SwitchPhase::ServiceStopping => {
                self.fault(FaultStage::ServiceStopping, HostKind::Service, reason)
            }
            SwitchPhase::AwaitingServiceRelease => self.fault(
                FaultStage::AwaitingServiceRelease,
                HostKind::Service,
                reason,
            ),
            SwitchPhase::DesktopStarting => {
                self.fault(FaultStage::DesktopStarting, HostKind::Offline, reason)
            }
        }
    }

    fn handle_switch_to_service(&mut self) -> Result<StepOutcome, SwitchError> {
        if !self.is_idle_or_terminal() {
            return Ok(StepOutcome::TransitionInProgress);
        }
        if self.state.current == HostKind::Service {
            return Ok(StepOutcome::NoOp);
        }
        if !self.can_operate_service {
            return Err(SwitchError::PermissionDenied);
        }
        self.state.service_fault = None;
        match self.state.current {
            HostKind::Desktop => match self.desktop.request_stop() {
                Ok(()) => {
                    self.enter_phase(SwitchPhase::AwaitingDesktopRelease);
                    Ok(StepOutcome::Progressed)
                }
                Err(err) => {
                    let reason = err.to_string();
                    Ok(self.fault(FaultStage::DesktopStopping, HostKind::Desktop, reason))
                }
            },
            HostKind::Offline => Ok(self.try_start_service()),
            HostKind::Service => unreachable!("already handled above"),
        }
    }

    fn handle_switch_to_desktop(&mut self) -> Result<StepOutcome, SwitchError> {
        if !self.is_idle_or_terminal() {
            return Ok(StepOutcome::TransitionInProgress);
        }
        if self.state.current == HostKind::Desktop {
            return Ok(StepOutcome::NoOp);
        }
        match self.state.current {
            HostKind::Service => {
                if !self.can_operate_service {
                    return Err(SwitchError::PermissionDenied);
                }
                self.state.service_fault = None;
                self.stop_retries = 0;
                Ok(self.issue_stop())
            }
            HostKind::Offline => {
                // 締めたまま止まった後の再要求: 終了コード 5 のままなら Desktop に渡さない
                // （relax-only で緩めて 0 になっていれば通す）。照会に失敗したら安全側で失敗。
                if self.state.service_fault == Some(ServiceFault::LeftTight) {
                    match self.service_manager.query_status() {
                        Ok(status) => {
                            if let Some(out) =
                                self.left_tight_fault(&status, "再要求時に SCM を確かめ直しました")
                            {
                                return Ok(out);
                            }
                            self.state.service_fault = None;
                        }
                        Err(err) => {
                            let reason = format!("再要求時の SCM 照会に失敗しました: {err}");
                            return Ok(self.fault(
                                FaultStage::AwaitingServiceRelease,
                                HostKind::Offline,
                                reason,
                            ));
                        }
                    }
                }
                Ok(self.attempt_desktop_start())
            }
            HostKind::Desktop => unreachable!("already handled above"),
        }
    }

    fn handle_cancel(&mut self) -> StepOutcome {
        if self.is_idle_or_terminal() {
            return StepOutcome::NoOp;
        }
        self.fault_for_current_phase("操作者によりキャンセルされました".to_string())
    }

    fn handle_poll(&mut self) -> StepOutcome {
        match self.state.phase.clone() {
            SwitchPhase::Idle => StepOutcome::NoOp,
            // 要求発行自体は同期 API なので `step` 内で即座に次段へ進み、
            // `Poll` 単独でここへ来ることは実運用上は起きない
            // （モジュール doc「DesktopStopping/ServiceStopping」節）。
            SwitchPhase::DesktopStopping => StepOutcome::Waiting,
            SwitchPhase::ServiceStopping => self.poll_service_stopping(),
            SwitchPhase::AwaitingDesktopRelease => self.poll_awaiting_desktop_release(),
            SwitchPhase::ServiceStarting => self.poll_service_starting(),
            SwitchPhase::AwaitingServiceHealth => self.poll_awaiting_service_health(),
            SwitchPhase::AwaitingServiceRelease => self.poll_awaiting_service_release(),
            SwitchPhase::DesktopStarting => self.poll_desktop_starting(),
            SwitchPhase::Faulted { stage, reason } => StepOutcome::Faulted {
                reached: self.state.current,
                stage,
                reason,
            },
        }
    }

    /// 不変条件1: ここに来る時点で`desktop.is_stopped()`（停止済み・mutex
    /// 解放済み）が確認できているので、これより前に`ServiceManager::start`を
    /// 呼ぶ経路は存在しない。
    fn try_start_service(&mut self) -> StepOutcome {
        match self.service_manager.start() {
            Ok(_handle) => {
                self.enter_phase(SwitchPhase::ServiceStarting);
                StepOutcome::Progressed
            }
            Err(err) => {
                let reason = describe_service_error(&err);
                self.fault(FaultStage::ServiceStarting, HostKind::Offline, reason)
            }
        }
    }

    fn poll_awaiting_desktop_release(&mut self) -> StepOutcome {
        if self.desktop.is_stopped() {
            self.state.current = HostKind::Offline;
            return self.try_start_service();
        }
        if self.timed_out() {
            let reason = self.timeout_reason();
            return self.fault_for_current_phase(reason);
        }
        StepOutcome::Waiting
    }

    /// START_PENDING の間は checkpoint が進む限り待つ（[`PendingWatch`]）。Running を経ずに
    /// Stopped になったら終了コードで分ける（4 = 運用モードにできない、5 = 締めたまま残った。
    /// どちらでもなければ従来どおり予期しない状態）。
    fn poll_service_starting(&mut self) -> StepOutcome {
        let status = match self.service_manager.query_status() {
            Ok(status) => status,
            Err(err) => {
                let reason = describe_service_error(&err);
                return self.fault_for_current_phase(reason);
            }
        };
        let now = self.now();
        let Some(watch) = self.watch.as_mut() else {
            return StepOutcome::Waiting;
        };
        match watch.observe(now, status) {
            WatchStep::Reached(_) => {
                self.enter_phase(SwitchPhase::AwaitingServiceHealth);
                StepOutcome::Progressed
            }
            WatchStep::Wait => StepOutcome::Waiting,
            WatchStep::Unexpected(status) => {
                if status.state == ScmState::Stopped {
                    if let Some(kind) = service_fault(&status) {
                        let reason = format!("サービスは Running にならずに止まりました: {kind}");
                        self.state.service_fault = Some(kind);
                        return self.fault_for_current_phase(reason);
                    }
                }
                let reason = format!("予期しない SCM 状態です: {}", status.state);
                self.fault_for_current_phase(reason)
            }
            step @ (WatchStep::Stalled | WatchStep::OverallTimeout) => {
                let reason = self.watch_expiry_reason(&step);
                self.fault_for_current_phase(reason)
            }
        }
    }

    /// 不変条件5: [`HealthOutcome::Healthy`]以外は成功と見なさない -
    /// `WrongProfileOrVersion`/`MutexOwnerUnknown`/`PortConflict`/
    /// `Unreachable`のいずれでも`Waiting`のまま（timeout まで）。
    fn poll_awaiting_service_health(&mut self) -> StepOutcome {
        let result = self.probe.probe(&self.expected_profile, self.expected_port);
        if let Ok(health) = &result {
            self.state.last_health = Some(health.clone());
        }
        if let Ok(HealthOutcome::Healthy { .. }) = &result {
            self.state.current = HostKind::Service;
            self.state.phase = SwitchPhase::Idle;
            self.phase_deadline = None;
            return StepOutcome::Completed(HostKind::Service);
        }
        if self.timed_out() {
            let reason = format!(
                "service health が {:?} 以内に確認できませんでした（最終 probe 結果: {result:?}）",
                self.phase_timeout
            );
            return self.fault_for_current_phase(reason);
        }
        StepOutcome::Waiting
    }

    /// 不変条件2: SCM `Stopped` **かつ** probe が`Unreachable`（旧 health
    /// 消失、mutex 解放の代替確認）を確認するまで Desktop 起動許可へ進まない。
    fn poll_awaiting_service_release(&mut self) -> StepOutcome {
        if let Some(step) = self.poll_service_released() {
            return step;
        }

        let health = self.probe.probe(&self.expected_profile, self.expected_port);
        if let Ok(h) = &health {
            self.state.last_health = Some(h.clone());
        }
        if matches!(health, Ok(HealthOutcome::Unreachable)) {
            self.state.current = HostKind::Offline;
            return self.attempt_desktop_start();
        }
        if self.timed_out() {
            let reason = format!(
                "service stop 後も旧 health が消失しませんでした（最終 probe 結果: {health:?}）"
            );
            return self.fault_for_current_phase(reason);
        }
        StepOutcome::Waiting
    }

    /// STOP を発行する。受け付けられたら`AwaitingServiceRelease`、1061／1052 で断られたら
    /// [`Self::stop_refused`]。
    fn issue_stop(&mut self) -> StepOutcome {
        match self.service_manager.stop() {
            Ok(_handle) => {
                self.enter_phase(SwitchPhase::AwaitingServiceRelease);
                StepOutcome::Progressed
            }
            Err(
                err @ (ServiceManagerError::CannotAcceptControl
                | ServiceManagerError::InvalidControl),
            ) => self.stop_refused(err),
            Err(err) => {
                let reason = describe_service_error(&err);
                self.fault(FaultStage::ServiceStopping, HostKind::Service, reason)
            }
        }
    }

    /// STOP が 1061（`CannotAcceptControl`）か 1052（`InvalidControl`）で断られた。権限の設定の
    /// 途中（START_PENDING／STOP_PENDING）は失敗にせず「切り替え中」として`ServiceStopping`で
    /// 待つ。1052 は照会し直した状態で分ける: 切り替え中・Running（起動が終わった競合）は待って
    /// 出し直し（[`MAX_STOP_RETRIES`]回まで）、Stopped は結果を取りに`AwaitingServiceRelease`へ。
    fn stop_refused(&mut self, err: ServiceManagerError) -> StepOutcome {
        if matches!(err, ServiceManagerError::InvalidControl) {
            match self.service_manager.query_status() {
                Ok(status) => match status.state {
                    ScmState::Stopped => {
                        self.enter_phase(SwitchPhase::AwaitingServiceRelease);
                        return StepOutcome::Progressed;
                    }
                    ScmState::StartPending | ScmState::StopPending | ScmState::Running => {}
                    _ => {
                        let reason = describe_service_error(&err);
                        return self.fault(FaultStage::ServiceStopping, HostKind::Service, reason);
                    }
                },
                Err(_) => {
                    let reason = describe_service_error(&err);
                    return self.fault(FaultStage::ServiceStopping, HostKind::Service, reason);
                }
            }
        }
        if self.stop_retries >= MAX_STOP_RETRIES {
            let reason = describe_service_error(&err);
            return self.fault(FaultStage::ServiceStopping, HostKind::Service, reason);
        }
        self.stop_retries += 1;
        self.enter_phase(SwitchPhase::ServiceStopping);
        StepOutcome::Progressed
    }

    /// `ServiceStopping`（STOP を断られて切り替え中を待っている）。Stopped になれば結果を取りに
    /// `AwaitingServiceRelease`へ、Running に戻ったら STOP を出し直す、pending の間は
    /// checkpoint が進む限り待つ。
    fn poll_service_stopping(&mut self) -> StepOutcome {
        let status = match self.service_manager.query_status() {
            Ok(status) => status,
            Err(err) => {
                let reason = describe_service_error(&err);
                return self.fault_for_current_phase(reason);
            }
        };
        let now = self.now();
        let Some(watch) = self.watch.as_mut() else {
            return StepOutcome::Waiting;
        };
        match watch.observe(now, status) {
            WatchStep::Reached(_) => {
                self.enter_phase(SwitchPhase::AwaitingServiceRelease);
                StepOutcome::Progressed
            }
            WatchStep::Wait => StepOutcome::Waiting,
            WatchStep::Unexpected(status) if status.state == ScmState::Running => self.issue_stop(),
            WatchStep::Unexpected(status) => {
                let reason = format!("予期しない SCM 状態です: {}", status.state);
                self.fault_for_current_phase(reason)
            }
            step @ (WatchStep::Stalled | WatchStep::OverallTimeout) => {
                let reason = self.watch_expiry_reason(&step);
                self.fault_for_current_phase(reason)
            }
        }
    }

    /// `AwaitingServiceRelease`の前半: SCM が`Stopped`になるまで、STOP_PENDING の間は
    /// checkpoint が進む限り待つ。`Stopped`が確認できたら`None`（probe の確認へ進む。以後の
    /// 期限は段の待ち時間で数え直す）。終了コード 5（締めたまま残った）は、デスクトップに
    /// 渡せない（プロファイルが読み取りだけのまま）ので`Offline`で失敗にする。
    fn poll_service_released(&mut self) -> Option<StepOutcome> {
        let status = match self.service_manager.query_status() {
            Ok(status) => status,
            Err(err) => {
                let reason = describe_service_error(&err);
                return Some(self.fault_for_current_phase(reason));
            }
        };
        let Some(mut watch) = self.watch.take() else {
            // Stopped は一度確認済み（probe の消失待ちの間）。それでも毎回 SCM を照会し、
            // 回復動作などで動き出していたら Unreachable を受け入れずに待つ。
            if status.state == ScmState::Stopped {
                // 動き出してから終了コード 5 で止まり直したら、締めたまま残っている。
                return self.left_tight_fault(&status, "サービスは止まり直しましたが");
            }
            if self.timed_out() {
                let reason = self.timeout_reason();
                return Some(self.fault_for_current_phase(reason));
            }
            return Some(StepOutcome::Waiting);
        };
        let now = self.now();
        match watch.observe(now, status) {
            WatchStep::Reached(status) => {
                self.phase_deadline = Some(now + self.phase_timeout);
                self.left_tight_fault(&status, "サービスは止まりましたが")
            }
            WatchStep::Wait => {
                self.watch = Some(watch);
                Some(StepOutcome::Waiting)
            }
            // Running など（STOP を受け付けたのに戻った）は、従来どおり段の期限まで待つ。
            WatchStep::Unexpected(_) => {
                self.watch = Some(watch);
                if self.timed_out() {
                    let reason = self.timeout_reason();
                    return Some(self.fault_for_current_phase(reason));
                }
                Some(StepOutcome::Waiting)
            }
            step @ (WatchStep::Stalled | WatchStep::OverallTimeout) => {
                let reason = self.watch_expiry_reason(&step);
                Some(self.fault_for_current_phase(reason))
            }
        }
    }

    /// 止まったサービスの終了コードが 5（締めたまま残った）なら、`service_fault`を載せて
    /// `Offline`で失敗にする。そうでなければ`None`（先へ進んでよい）。
    fn left_tight_fault(
        &mut self,
        status: &ServiceStatusSummary,
        prefix: &str,
    ) -> Option<StepOutcome> {
        let kind @ ServiceFault::LeftTight = service_fault(status)? else {
            return None;
        };
        let reason = format!("{prefix}: {kind}");
        self.state.service_fault = Some(kind);
        Some(self.fault(
            FaultStage::AwaitingServiceRelease,
            HostKind::Offline,
            reason,
        ))
    }

    /// Desktop の起動を即座に許可できるか確認する - 許可されれば
    /// [`HostKind::Desktop`]へ即完了、されなければ`DesktopStarting`で
    /// ポーリング待ちに入る（実際の`HubRuntime::start`はシェル側の責務、
    /// モジュール doc）。
    fn attempt_desktop_start(&mut self) -> StepOutcome {
        if self.desktop.request_start_allowed() {
            self.state.current = HostKind::Desktop;
            self.state.phase = SwitchPhase::Idle;
            self.phase_deadline = None;
            StepOutcome::Completed(HostKind::Desktop)
        } else {
            self.enter_phase(SwitchPhase::DesktopStarting);
            StepOutcome::Progressed
        }
    }

    fn poll_desktop_starting(&mut self) -> StepOutcome {
        if self.desktop.request_start_allowed() {
            self.state.current = HostKind::Desktop;
            self.state.phase = SwitchPhase::Idle;
            self.phase_deadline = None;
            return StepOutcome::Completed(HostKind::Desktop);
        }
        if self.timed_out() {
            let reason = self.timeout_reason();
            return self.fault_for_current_phase(reason);
        }
        StepOutcome::Waiting
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hub_health::MockHubHealthProbe;
    use crate::service_manager::MockServiceManager;

    const TEST_PROFILE: &str = "default";
    const TEST_PORT: u16 = 8722;

    fn engine_with(
        current: HostKind,
        can_operate_service: bool,
        timeout: Duration,
    ) -> HostSwitchEngine<MockServiceManager, MockHubHealthProbe, MockDesktopHostControl> {
        let service_manager = match current {
            HostKind::Service => MockServiceManager::new(),
            _ => MockServiceManager::new(),
        };
        if current == HostKind::Service {
            service_manager.start().expect("prime service running");
        }
        HostSwitchEngine::new(
            service_manager,
            MockHubHealthProbe::unreachable(),
            MockDesktopHostControl::new(),
            HostSwitchConfig {
                expected_profile: TEST_PROFILE.to_string(),
                expected_port: TEST_PORT,
                can_operate_service,
                initial_host: current,
                phase_timeout: timeout,
            },
        )
    }

    // 不変条件1: Service start() を呼ぶ前に Desktop が stopped かつ
    // mutex released（モック上は is_stopped が両方を表す）でなければならず、
    // health が確認できるまで current は Service へ進まない。
    #[test]
    fn desktop_to_service_happy_path_respects_ordering() {
        let mut engine = engine_with(HostKind::Desktop, true, Duration::from_secs(5));

        let outcome = engine
            .step(SwitchCommand::SwitchToService)
            .expect("switch should be accepted");
        assert_eq!(outcome, StepOutcome::Progressed);
        assert_eq!(engine.state().phase, SwitchPhase::AwaitingDesktopRelease);

        // Desktop がまだ停止していない間、Service は Stopped のままのはず。
        assert_eq!(
            engine.step(SwitchCommand::Poll).expect("poll ok"),
            StepOutcome::Waiting
        );
        assert_eq!(engine.service_manager.current_state(), ScmState::Stopped);

        // stopped=true だが mutex_released=false の間もまだ start しない。
        engine.desktop.set_stopped(true);
        assert_eq!(
            engine.step(SwitchCommand::Poll).expect("poll ok"),
            StepOutcome::Waiting
        );
        assert_eq!(engine.service_manager.current_state(), ScmState::Stopped);

        // 両方満たされて初めて start() が呼ばれる。
        engine.desktop.set_mutex_released(true);
        assert_eq!(
            engine.step(SwitchCommand::Poll).expect("poll ok"),
            StepOutcome::Progressed
        );
        assert_eq!(engine.service_manager.current_state(), ScmState::Running);
        // health 確認前はまだ Offline（Service を named/named 完了と見なさない）。
        assert_eq!(engine.state().current, HostKind::Offline);
        assert_eq!(engine.state().phase, SwitchPhase::ServiceStarting);

        // SCM は即座に Running へ落ち着くモックなので、次の poll で health 待ちへ。
        assert_eq!(
            engine.step(SwitchCommand::Poll).expect("poll ok"),
            StepOutcome::Progressed
        );
        assert_eq!(engine.state().phase, SwitchPhase::AwaitingServiceHealth);
        assert_eq!(engine.state().current, HostKind::Offline);

        engine.probe.set_outcome(HealthOutcome::Healthy {
            version: "1.2.3".to_string(),
        });
        assert_eq!(
            engine.step(SwitchCommand::Poll).expect("poll ok"),
            StepOutcome::Completed(HostKind::Service)
        );
        assert_eq!(engine.state().current, HostKind::Service);
        assert_eq!(engine.state().phase, SwitchPhase::Idle);
    }

    // 不変条件2: Desktop start 許可を出す前に Service が Stopped かつ
    // 旧 health 消失（probe Unreachable）を確認しなければならない。
    #[test]
    fn service_to_desktop_happy_path_respects_ordering() {
        let mut engine = engine_with(HostKind::Service, true, Duration::from_secs(5));
        engine.probe.set_outcome(HealthOutcome::Healthy {
            version: "1.2.3".to_string(),
        });

        assert_eq!(
            engine.step(SwitchCommand::SwitchToDesktop).expect("ok"),
            StepOutcome::Progressed
        );
        assert_eq!(engine.state().phase, SwitchPhase::AwaitingServiceRelease);
        assert_eq!(engine.service_manager.current_state(), ScmState::Stopped);

        // SCM は Stopped になったが、旧 health がまだ応答している間は待つ
        // （current は Service のまま = Desktop を起動許可しない）。
        assert_eq!(
            engine.step(SwitchCommand::Poll).expect("poll ok"),
            StepOutcome::Waiting
        );
        assert_eq!(engine.state().current, HostKind::Service);

        engine.probe.set_outcome(HealthOutcome::Unreachable);
        assert_eq!(
            engine.step(SwitchCommand::Poll).expect("poll ok"),
            StepOutcome::Completed(HostKind::Desktop)
        );
        assert_eq!(engine.state().current, HostKind::Desktop);
        assert_eq!(engine.state().phase, SwitchPhase::Idle);
    }

    // #699 レビュー: 一度 Stopped を観測した後も毎回 SCM を照会する。回復動作などで
    // サービスが動き出したときの一時的な Unreachable では Desktop へ進まない。
    #[test]
    fn service_release_requires_stopped_on_every_poll_after_first_stopped() {
        let mut engine = engine_with(HostKind::Service, true, Duration::from_secs(5));
        engine.probe.set_outcome(HealthOutcome::Healthy {
            version: "1.2.3".to_string(),
        });
        engine.step(SwitchCommand::SwitchToDesktop).expect("ok");
        assert_eq!(engine.state().phase, SwitchPhase::AwaitingServiceRelease);

        // 最初の Stopped を観測（旧 health はまだ応答中）。
        assert_eq!(
            engine.step(SwitchCommand::Poll).expect("poll ok"),
            StepOutcome::Waiting
        );

        // 回復動作でサービスが再び動き出し、起動途中で probe が Unreachable。
        engine.service_manager.start().expect("restart");
        engine.probe.set_outcome(HealthOutcome::Unreachable);
        assert_eq!(
            engine.step(SwitchCommand::Poll).expect("poll ok"),
            StepOutcome::Waiting
        );
        assert_eq!(engine.state().current, HostKind::Service);
        assert_eq!(engine.state().phase, SwitchPhase::AwaitingServiceRelease);

        // 再び Stopped かつ Unreachable になれば進む。
        engine.service_manager.stop().expect("stop again");
        assert_eq!(
            engine.step(SwitchCommand::Poll).expect("poll ok"),
            StepOutcome::Completed(HostKind::Desktop)
        );
    }

    // 不変条件3: 失敗時に「もう一方を起動したまま戻る」ことがない -
    // Desktop 解放後に Service start が失敗したら Offline（両方停止）で
    // 落ち着き、Desktop が勝手に再起動されることもない。
    #[test]
    fn desktop_to_service_failure_at_service_start_lands_on_offline_not_desktop() {
        let mut engine = engine_with(HostKind::Desktop, true, Duration::from_secs(5));
        engine.step(SwitchCommand::SwitchToService).unwrap();
        engine.desktop.set_released(true);
        // Desktop 解放が確認された直後に start() が呼ばれる - ここで
        // 権限エラーを注入して失敗させる。
        engine
            .service_manager
            .inject_error(crate::service_manager::ServiceManagerError::AccessDenied);

        let first_outcome = engine.step(SwitchCommand::Poll).expect("poll ok");
        let reason = match &first_outcome {
            StepOutcome::Faulted {
                reached,
                stage,
                reason,
            } => {
                assert_eq!(*reached, HostKind::Offline);
                assert_eq!(*stage, FaultStage::ServiceStarting);
                // 利用者に見える理由は B2-3 までと同じ banto-hub の文言
                // （B2-4a: 共有 crate の`Display`は中立になったが、ここは
                // `describe_service_error`を通す）。
                assert_eq!(
                    reason,
                    "banto-hub: サービス操作の権限がありません（管理者権限が必要です）"
                );
                reason.clone()
            }
            other => panic!("expected Faulted, got {other:?}"),
        };
        assert_eq!(engine.state().current, HostKind::Offline);
        assert!(matches!(engine.state().phase, SwitchPhase::Faulted { .. }));

        // Faulted は終端 - 明示コマンドが来るまで、Poll を重ねても同じ
        // Faulted をそのまま返すだけで、何も新しく進めない。
        assert_eq!(
            engine.step(SwitchCommand::Poll).expect("poll ok"),
            StepOutcome::Faulted {
                reached: HostKind::Offline,
                stage: FaultStage::ServiceStarting,
                reason,
            }
        );
    }

    // 不変条件4: 遷移中（Idle/Faulted 以外）は新しい SwitchTo* を重ねない。
    #[test]
    fn overlapping_switch_to_desktop_during_progress_is_rejected() {
        let mut engine = engine_with(HostKind::Desktop, true, Duration::from_secs(5));
        engine.step(SwitchCommand::SwitchToService).unwrap();
        assert_eq!(engine.state().phase, SwitchPhase::AwaitingDesktopRelease);

        // 既に進行中なので、逆方向であっても新しい要求は無視される。
        assert_eq!(
            engine.step(SwitchCommand::SwitchToDesktop).unwrap(),
            StepOutcome::TransitionInProgress
        );
        // 同方向の多重クリックも同様。
        assert_eq!(
            engine.step(SwitchCommand::SwitchToService).unwrap(),
            StepOutcome::TransitionInProgress
        );
        // 状態自体は変化していない。
        assert_eq!(engine.state().current, HostKind::Desktop);
        assert_eq!(engine.state().phase, SwitchPhase::AwaitingDesktopRelease);
    }

    // 不変条件5: probe が WrongProfileOrVersion/MutexOwnerUnknown/
    // PortConflict の間は Service 開始成功と見なさない。
    #[test]
    fn ambiguous_probe_outcomes_never_complete_the_switch() {
        let mut engine = engine_with(HostKind::Desktop, true, Duration::from_secs(5));
        engine.step(SwitchCommand::SwitchToService).unwrap();
        engine.desktop.set_released(true);
        engine.step(SwitchCommand::Poll).unwrap(); // start() 発行
        engine.step(SwitchCommand::Poll).unwrap(); // ServiceStarting -> AwaitingServiceHealth

        for outcome in [
            HealthOutcome::WrongProfileOrVersion,
            HealthOutcome::MutexOwnerUnknown,
            HealthOutcome::PortConflict,
            HealthOutcome::Unreachable,
        ] {
            engine.probe.set_outcome(outcome.clone());
            assert_eq!(
                engine.step(SwitchCommand::Poll).unwrap(),
                StepOutcome::Waiting
            );
            assert_eq!(engine.state().current, HostKind::Offline);
            assert_eq!(engine.state().last_health, Some(outcome));
        }

        engine.probe.set_outcome(HealthOutcome::Healthy {
            version: "9.9.9".to_string(),
        });
        assert_eq!(
            engine.step(SwitchCommand::Poll).unwrap(),
            StepOutcome::Completed(HostKind::Service)
        );
    }

    #[test]
    fn permission_denied_stub_blocks_service_start_without_calling_scm() {
        let mut engine = engine_with(HostKind::Desktop, false, Duration::from_secs(5));
        let result = engine.step(SwitchCommand::SwitchToService);
        assert!(matches!(result, Err(SwitchError::PermissionDenied)));
        // 状態も SCM も一切変化していない。
        assert_eq!(engine.state().phase, SwitchPhase::Idle);
        assert_eq!(engine.state().current, HostKind::Desktop);
        assert_eq!(engine.service_manager.current_state(), ScmState::Stopped);
        assert!(!engine.desktop.stop_was_requested());
    }

    #[test]
    fn permission_denied_stub_blocks_service_stop_without_calling_scm() {
        let mut engine = engine_with(HostKind::Service, false, Duration::from_secs(5));
        let result = engine.step(SwitchCommand::SwitchToDesktop);
        assert!(matches!(result, Err(SwitchError::PermissionDenied)));
        assert_eq!(engine.state().phase, SwitchPhase::Idle);
        assert_eq!(engine.state().current, HostKind::Service);
        assert_eq!(engine.service_manager.current_state(), ScmState::Running);
    }

    #[test]
    fn switch_to_service_when_already_service_is_idempotent_noop() {
        let mut engine = engine_with(HostKind::Service, true, Duration::from_secs(5));
        assert_eq!(
            engine.step(SwitchCommand::SwitchToService).unwrap(),
            StepOutcome::NoOp
        );
        assert_eq!(engine.state().phase, SwitchPhase::Idle);
    }

    #[test]
    fn switch_to_desktop_when_already_desktop_is_idempotent_noop() {
        let mut engine = engine_with(HostKind::Desktop, true, Duration::from_secs(5));
        assert_eq!(
            engine.step(SwitchCommand::SwitchToDesktop).unwrap(),
            StepOutcome::NoOp
        );
        assert_eq!(engine.state().phase, SwitchPhase::Idle);
    }

    #[test]
    fn desktop_stop_request_failure_leaves_desktop_as_current() {
        let mut engine = engine_with(HostKind::Desktop, true, Duration::from_secs(5));
        engine
            .desktop
            .inject_stop_error(DesktopHostError::Other("shutdown hook failed".to_string()));

        let outcome = engine.step(SwitchCommand::SwitchToService).unwrap();
        match outcome {
            StepOutcome::Faulted { reached, stage, .. } => {
                assert_eq!(reached, HostKind::Desktop);
                assert_eq!(stage, FaultStage::DesktopStopping);
            }
            other => panic!("expected Faulted, got {other:?}"),
        }
        assert_eq!(engine.state().current, HostKind::Desktop);

        // Faulted からの再試行は許される（is_idle_or_terminal）。
        let retry = engine.step(SwitchCommand::SwitchToService).unwrap();
        assert_eq!(retry, StepOutcome::Progressed);
        assert_eq!(engine.state().phase, SwitchPhase::AwaitingDesktopRelease);
    }

    #[test]
    fn service_stop_request_failure_leaves_service_as_current() {
        let mut engine = engine_with(HostKind::Service, true, Duration::from_secs(5));
        engine
            .service_manager
            .inject_error(crate::service_manager::ServiceManagerError::AccessDenied);

        let outcome = engine.step(SwitchCommand::SwitchToDesktop).unwrap();
        match outcome {
            StepOutcome::Faulted { reached, stage, .. } => {
                assert_eq!(reached, HostKind::Service);
                assert_eq!(stage, FaultStage::ServiceStopping);
            }
            other => panic!("expected Faulted, got {other:?}"),
        }
        assert_eq!(engine.state().current, HostKind::Service);
    }

    #[test]
    fn cancel_during_awaiting_desktop_release_lands_safely_on_desktop() {
        let mut engine = engine_with(HostKind::Desktop, true, Duration::from_secs(5));
        engine.step(SwitchCommand::SwitchToService).unwrap();
        assert_eq!(engine.state().phase, SwitchPhase::AwaitingDesktopRelease);

        let outcome = engine.step(SwitchCommand::Cancel).unwrap();
        match outcome {
            StepOutcome::Faulted { reached, stage, .. } => {
                assert_eq!(reached, HostKind::Desktop);
                assert_eq!(stage, FaultStage::AwaitingDesktopRelease);
            }
            other => panic!("expected Faulted, got {other:?}"),
        }
        assert_eq!(engine.state().current, HostKind::Desktop);
    }

    #[test]
    fn cancel_when_idle_is_a_noop() {
        let mut engine = engine_with(HostKind::Desktop, true, Duration::from_secs(5));
        assert_eq!(
            engine.step(SwitchCommand::Cancel).unwrap(),
            StepOutcome::NoOp
        );
        assert_eq!(engine.state().phase, SwitchPhase::Idle);
    }

    #[test]
    fn awaiting_desktop_release_timeout_lands_on_desktop_without_touching_service() {
        let mut engine = engine_with(HostKind::Desktop, true, Duration::from_millis(20));
        engine.step(SwitchCommand::SwitchToService).unwrap();
        // desktop はいつまでも停止しない。
        std::thread::sleep(Duration::from_millis(30));

        let outcome = engine.step(SwitchCommand::Poll).unwrap();
        match outcome {
            StepOutcome::Faulted { reached, stage, .. } => {
                assert_eq!(reached, HostKind::Desktop);
                assert_eq!(stage, FaultStage::AwaitingDesktopRelease);
            }
            other => panic!("expected Faulted, got {other:?}"),
        }
        // Service には一度も start が呼ばれていない（不変条件1）。
        assert_eq!(engine.service_manager.current_state(), ScmState::Stopped);
    }

    #[test]
    fn awaiting_service_health_timeout_with_scm_running_reports_service_unhealthy() {
        let mut engine = engine_with(HostKind::Desktop, true, Duration::from_millis(20));
        engine.step(SwitchCommand::SwitchToService).unwrap();
        engine.desktop.set_released(true);
        engine.step(SwitchCommand::Poll).unwrap(); // start() 発行 -> ServiceStarting
        engine.step(SwitchCommand::Poll).unwrap(); // -> AwaitingServiceHealth

        // probe が Unreachable を返し続け、timeout まで待つ。
        std::thread::sleep(Duration::from_millis(30));
        let outcome = engine.step(SwitchCommand::Poll).unwrap();
        match outcome {
            StepOutcome::Faulted { reached, stage, .. } => {
                assert_eq!(
                    reached,
                    HostKind::Service,
                    "SCM が Running のままなら Service 扱い"
                );
                assert_eq!(stage, FaultStage::AwaitingServiceHealth);
            }
            other => panic!("expected Faulted, got {other:?}"),
        }
        assert_eq!(engine.state().current, HostKind::Service);
        // Running のまま health 不明という「Service だが Unhealthy」を表す。
        assert_eq!(engine.state().last_health, Some(HealthOutcome::Unreachable));
    }

    #[test]
    fn offline_to_service_skips_desktop_stop_phase() {
        let mut engine = engine_with(HostKind::Offline, true, Duration::from_secs(5));
        assert_eq!(
            engine.step(SwitchCommand::SwitchToService).unwrap(),
            StepOutcome::Progressed
        );
        assert_eq!(engine.state().phase, SwitchPhase::ServiceStarting);
        assert!(!engine.desktop.stop_was_requested());
    }

    #[test]
    fn offline_to_desktop_waits_for_start_permission_then_completes() {
        let mut engine = engine_with(HostKind::Offline, true, Duration::from_secs(5));
        engine.desktop.set_start_allowed(false);

        assert_eq!(
            engine.step(SwitchCommand::SwitchToDesktop).unwrap(),
            StepOutcome::Progressed
        );
        assert_eq!(engine.state().phase, SwitchPhase::DesktopStarting);

        assert_eq!(
            engine.step(SwitchCommand::Poll).unwrap(),
            StepOutcome::Waiting
        );

        engine.desktop.set_start_allowed(true);
        assert_eq!(
            engine.step(SwitchCommand::Poll).unwrap(),
            StepOutcome::Completed(HostKind::Desktop)
        );
        assert_eq!(engine.state().current, HostKind::Desktop);
    }
}

#[cfg(test)]
mod pending_tests;
