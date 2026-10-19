//! #661 661-5: シェルが profile の形を見てから Desktop を許す判定と、Fallback の窓の選び方
//! （docs/banto-hub-t17-design.md §1 の「2026-10-19 オーナー決定（#661）」の「画面」と
//! 「#661 661-5 実装メモ」）。
//!
//! **純関数だけ**を置く（どの OS でもテストする）。SCM・ファイル・窓には触れない（観測の
//! [`observe_profile_mode`] だけは profile を読むだけで開く。非 Windows は観測しない）。手本は
//! ChronoGazer の `chronogazer_core::shell_launch::decide`・`observe_root_mode` と
//! `apps/chronogazer/src-tauri/src/shell.rs` の `lost_cause`・`FallbackCause`（#623 PR-4・#686〜#690）。
//!
//! ## 約束
//!
//! - **profile が運用モードの形（締まっている）なら、決して Desktop にしない**（[`plan_startup`]）。
//!   SCM の照会に失敗したとき（従来は Desktop を試す）もこの中に入る。サービスが運用中に
//!   締めたまま止まった（PRESHUTDOWN の後の 0・scan より前の 2・3・5）、シェルを起動し直した
//!   （新しい engine は過去の終了コードを知らない）のどれでも、profile の形そのものを見るので守れる。
//! - 判定に使うのは profile の形と SCM の状態だけで、終了コード（4・5）は**文言を選ぶため**にだけ
//!   使う（[`ServiceFault`]）。形が緩んでいれば（管理者が直した後など）、5 が残っていても Desktop へ進む。
//! - 断るのは運用モードの形と、profile を観測に使えない（リンク・読む途中の失敗）ときだけ。設定
//!   モードの形・無い・どちらの形でもない・読めない（自分の ACE が無い）は Desktop へ進み、Hub の
//!   起動（profile の排他・DB を開く）が最後の防衛線（ChronoGazer と同じ）。

use std::path::Path;

use banto_hub_core::host_switch::{RelaxOnlyOutcome, ServiceFault, SwitchPhase};
use banto_hub_core::profile_mode::ProfileShape;
use banto_hub_core::service_manager::{ScmState, ServiceEnd};

// ---------------------------------------------------------------------------
// profile の形の観測
// ---------------------------------------------------------------------------

/// profile のフォルダの形の観測（デスクトップの利用者の権限で、DACL を読むだけ）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ProfileModeObservation {
    /// 読めた（[`ProfileShape`]。無い・読めないも含む）。
    Shape(ProfileShape),
    /// profile を観測に使えない（reparse point・フォルダでない・読む途中の失敗。中身は理由）。
    Unusable(String),
    /// 観測していない（Windows でない）。
    NotObserved,
}

/// Desktop を断る理由。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ProfileBlock {
    /// profile が運用モードの形（締まっている）。
    OperationMode,
    /// profile を観測に使えない（理由の文言）。
    Unusable(String),
}

impl ProfileModeObservation {
    /// Desktop を断る理由（断らないなら `None`）。
    pub(crate) fn blocks_desktop(&self) -> Option<ProfileBlock> {
        match self {
            Self::Shape(ProfileShape::Tight) => Some(ProfileBlock::OperationMode),
            Self::Unusable(reason) => Some(ProfileBlock::Unusable(reason.clone())),
            Self::Shape(_) | Self::NotObserved => None,
        }
    }
}

/// profile の形を観測する（Windows: profile を読むだけで開き、辿らない。作らない・書かない。
/// 非 Windows: [`ProfileModeObservation::NotObserved`]）。サービス本体の `observe_root` →
/// `profile_mode::profile_shape` と同じ判定を使う（「profile が締まっている ⇔ 運用中か切り替えの途中」）。
pub(crate) fn observe_profile_mode(profile_dir: &Path) -> ProfileModeObservation {
    #[cfg(windows)]
    {
        match banto_safe_root::mode::observe_root(profile_dir) {
            Ok(observed) => ProfileModeObservation::Shape(
                banto_hub_core::profile_mode::profile_shape(&observed),
            ),
            Err(err) => ProfileModeObservation::Unusable(err.to_string()),
        }
    }
    #[cfg(not(windows))]
    {
        let _ = profile_dir;
        ProfileModeObservation::NotObserved
    }
}

// ---------------------------------------------------------------------------
// Fallback の理由
// ---------------------------------------------------------------------------

/// Fallback になった理由（窓とトレイの文言・操作を選ぶため）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FallbackCause {
    /// 一般（起動の判定でサービスが遷移中・不健康、切り替えの失敗、Hub の起動の失敗）。
    General,
    /// サービスに接続していたら、サービスが止まった（見張り）。
    ServiceLost,
    /// profile が運用モードの形で、サービスは止まっている。`left_tight` は最後の停止の
    /// 終了コードが 5（締めたまま残った）。
    OperationStopped { left_tight: bool },
    /// profile を観測に使えない（リンクなど）。
    ProfileUnusable,
    /// 運用モードへの切り替えで、サービスが 4（運用モードにできなかった）で止まった。
    /// ファイルは変わっておらず、設定モードのまま。
    CannotOperate,
}

impl FallbackCause {
    /// モードに関わる理由か（サービスが動き出したら意味を失う - [`carry_cause`]）。
    fn is_mode_cause(self) -> bool {
        matches!(
            self,
            Self::OperationStopped { .. } | Self::ProfileUnusable | Self::CannotOperate
        )
    }

    /// 画面（`/status`）へ渡す名前（一般は `None`）。
    pub(crate) fn key(self) -> Option<&'static str> {
        match self {
            Self::General => None,
            Self::ServiceLost => Some("serviceLost"),
            Self::OperationStopped { left_tight: false } => Some("operationStopped"),
            Self::OperationStopped { left_tight: true } => Some("leftTight"),
            Self::ProfileUnusable => Some("profileUnusable"),
            Self::CannotOperate => Some("cannotOperate"),
        }
    }

    /// 「設定モードに戻す」 を出すか（profile が締まったまま止まっているときだけ）。
    pub(crate) fn offers_restore(self) -> bool {
        matches!(self, Self::OperationStopped { .. })
    }
}

/// [`ProfileBlock`] から理由を選ぶ。
pub(crate) fn blocked_cause(why: &ProfileBlock, fault: Option<ServiceFault>) -> FallbackCause {
    match why {
        ProfileBlock::OperationMode => FallbackCause::OperationStopped {
            left_tight: fault == Some(ServiceFault::LeftTight),
        },
        ProfileBlock::Unusable(_) => FallbackCause::ProfileUnusable,
    }
}

/// 切り替え（`HostSwitchEngine`）の失敗の理由（`service_fault` から）。
pub(crate) fn switch_fault_cause(fault: Option<ServiceFault>) -> FallbackCause {
    match fault {
        Some(ServiceFault::CannotOperate) => FallbackCause::CannotOperate,
        Some(ServiceFault::LeftTight) => FallbackCause::OperationStopped { left_tight: true },
        None => FallbackCause::General,
    }
}

// ---------------------------------------------------------------------------
// 起動の判定
// ---------------------------------------------------------------------------

/// 起動・再試行・切り替えの後に Desktop へ進む直前の判定の結果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum StartupPlan {
    /// サービスが Running: health を確かめてつなぐ（従来どおり）。
    ProbeService,
    /// このアプリで Hub を起動してよい。
    TryDesktop,
    /// Fallback の窓を出す。
    Fallback(FallbackCause),
}

/// 起動の判定（純関数）。`scm` は SCM の状態（照会の失敗・非 Windows は `None`）、`fault` は
/// SCM が停止のときの最後の終了の理由（4・5）。**Desktop になる組合せだけ** profile の形を見る。
pub(crate) fn plan_startup(
    scm: Option<&ScmState>,
    mode: &ProfileModeObservation,
    fault: Option<ServiceFault>,
) -> StartupPlan {
    match scm {
        Some(ScmState::Running) => StartupPlan::ProbeService,
        Some(ScmState::StartPending | ScmState::StopPending | ScmState::Other(_)) => {
            StartupPlan::Fallback(FallbackCause::General)
        }
        Some(ScmState::Stopped | ScmState::NotInstalled) | None => match mode.blocks_desktop() {
            Some(why) => StartupPlan::Fallback(blocked_cause(&why, fault)),
            None => StartupPlan::TryDesktop,
        },
    }
}

// ---------------------------------------------------------------------------
// サービスの見張り
// ---------------------------------------------------------------------------

/// サービスが止まったときの理由（純関数。#687）。SCM が停止のときだけ終了コードを見る:
/// 4 → `CannotOperate`、5 → `OperationStopped { left_tight: true }`、それ以外は profile の形で
/// 運用モードのまま停止なら `OperationStopped { left_tight: false }`・観測に使えないなら
/// `ProfileUnusable`、どちらでもなければ一般の `ServiceLost`。
pub(crate) fn lost_cause(
    scm: Option<&ScmState>,
    mode: &ProfileModeObservation,
    fault: Option<ServiceFault>,
) -> FallbackCause {
    if scm != Some(&ScmState::Stopped) {
        return FallbackCause::ServiceLost;
    }
    match fault {
        Some(ServiceFault::CannotOperate) => FallbackCause::CannotOperate,
        Some(ServiceFault::LeftTight) => FallbackCause::OperationStopped { left_tight: true },
        None => match mode.blocks_desktop() {
            Some(why) => blocked_cause(&why, None),
            None => FallbackCause::ServiceLost,
        },
    }
}

/// Fallback のまま表示を更新するときの理由（純関数）。モードに関わる理由は、サービスが実行中・
/// 開始や停止の途中になったら一般に戻す（誰かが運用を再開したのに「停止中」と出し続けない）。
pub(crate) fn carry_cause(cause: FallbackCause, scm: Option<&ScmState>) -> FallbackCause {
    let moving = matches!(
        scm,
        Some(
            ScmState::Running | ScmState::StartPending | ScmState::StopPending | ScmState::Other(_)
        )
    );
    if cause.is_mode_cause() && moving {
        FallbackCause::General
    } else {
        cause
    }
}

/// 見張りが見る画面。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WatchedView {
    Desktop,
    Service,
    Fallback(FallbackCause),
}

/// 見張りの 1 回の結果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WatchStep {
    Stay,
    /// サービスの画面から Fallback へ。
    ToFallback(FallbackCause),
    /// Fallback のまま理由を変える。
    Refresh(FallbackCause),
}

/// 見張りの 1 回（純関数）。**Desktop は決して返さない**（自動では Hub を起動しない）。`mode` は
/// SCM が停止のときだけ見る（それ以外は観測しなくてよい - 呼び出し側は `NotObserved` を渡せる）。
///
/// サービスを外から STOP すると `Running → StopPending → Stopped` と観測する。StopPending の時点では
/// 一般の `ServiceLost` で Fallback に移るので、**停止に達したら終了コードと形で理由を選び直す**
/// （#690 レビュー。4・5 の窓）。他の理由（利用者の操作の後の窓など）は書き換えない。
pub(crate) fn on_watch_tick(
    view: WatchedView,
    scm: Option<&ScmState>,
    mode: &ProfileModeObservation,
    fault: Option<ServiceFault>,
) -> WatchStep {
    match view {
        WatchedView::Desktop => WatchStep::Stay,
        WatchedView::Service => match scm {
            // 照会の失敗は一時的かもしれないので、そのまま（次の回でまた見る）。
            Some(ScmState::Running) | None => WatchStep::Stay,
            Some(_) => WatchStep::ToFallback(lost_cause(scm, mode, fault)),
        },
        WatchedView::Fallback(current) => {
            let next = if current == FallbackCause::ServiceLost && scm == Some(&ScmState::Stopped) {
                lost_cause(scm, mode, fault)
            } else {
                carry_cause(current, scm)
            };
            if next == current {
                WatchStep::Stay
            } else {
                WatchStep::Refresh(next)
            }
        }
    }
}

// ---------------------------------------------------------------------------
// 「設定モードに戻す」
// ---------------------------------------------------------------------------

/// 緩めるだけの開始（`relax-only`）の結果の扱い。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AfterRelax {
    /// 緩め終わった（0）: 起動の判定（[`plan_startup`]）で次を決める。
    Decide,
    /// 戻らなかった: 昇格ヘルパーの修復（`grant-profile-acl`、UAC）へ。`left_tight` は
    /// サービスが 5 で止まった（デスクトップのログに案内を書く）。
    Elevate { left_tight: bool },
    /// サービスが動いている（停止中ではなかった）: 知らせるだけ（修復もしない）。
    Running(String),
}

/// 緩めるだけの開始の結果から次を決める（純関数。ChronoGazer の `after_relax` と同じ表）。
/// 戻らなかった理由が何であれ（5・未登録・権限が無い・固まった）、最後の手段は昇格ヘルパーの
/// 修復。サービスが動いているときだけは修復しない（昇格ヘルパーも稼働中は断る）。
pub(crate) fn after_relax(outcome: &RelaxOnlyOutcome) -> AfterRelax {
    match outcome {
        RelaxOnlyOutcome::Done(ServiceEnd::Clean) => AfterRelax::Decide,
        RelaxOnlyOutcome::NotStopped(state) => AfterRelax::Running(format!(
            "サービスが動いています（{state}）。止まってから、もう一度押してください。"
        )),
        RelaxOnlyOutcome::Done(ServiceEnd::LeftOperation) => {
            AfterRelax::Elevate { left_tight: true }
        }
        RelaxOnlyOutcome::Done(_)
        | RelaxOnlyOutcome::NotInstalled
        | RelaxOnlyOutcome::Refused(_)
        | RelaxOnlyOutcome::Unexpected(_)
        | RelaxOnlyOutcome::Stalled
        | RelaxOnlyOutcome::TimedOut => AfterRelax::Elevate { left_tight: false },
    }
}

// ---------------------------------------------------------------------------
// 文言
// ---------------------------------------------------------------------------

/// 「切り替えの間は収集が止まります」の一言（窓・確認・`/status` で同じ文）。
pub(crate) const PAUSE_NOTE: &str = "切り替えの間は収集が止まります。";

/// 切り替えの段ごとの進み具合の文（`/status` の進み具合。段が変わったときに出す）。サービスの
/// 開始・停止の段は、checkpoint が進む間は待ちを延ばすので、その間ずっとこの文が出る。
pub(crate) fn switch_phase_message(phase: &SwitchPhase) -> Option<&'static str> {
    match phase {
        SwitchPhase::DesktopStopping | SwitchPhase::AwaitingDesktopRelease => {
            Some("このアプリの収集を止めています…")
        }
        SwitchPhase::ServiceStarting => {
            Some("サービスを開始し、ファイルの権限を設定しています（運用モードへ）…")
        }
        SwitchPhase::AwaitingServiceHealth => Some("サービスの応答を待っています…"),
        SwitchPhase::ServiceStopping | SwitchPhase::AwaitingServiceRelease => {
            Some("サービスを止め、ファイルの権限を元に戻しています（設定モードへ）…")
        }
        SwitchPhase::DesktopStarting => Some("このアプリで Hub を起動する準備をしています…"),
        SwitchPhase::Idle | SwitchPhase::Faulted { .. } => None,
    }
}

/// 「設定モードに戻す」 の段の文。
pub(crate) const RESTORE_RELAXING_MESSAGE: &str =
    "サービスを relax-only で開始し、ファイルの権限を元に戻しています…";
/// 昇格ヘルパーで修復する段の文。
pub(crate) const RESTORE_REPAIRING_MESSAGE: &str =
    "管理者の許可（ユーザーアカウント制御）を求めて、ファイルの権限を直しています…";

/// Fallback の窓の見出しと本文（一般は `None` で、従来の診断の行だけを出す）。
pub(crate) fn cause_page(cause: FallbackCause) -> Option<(&'static str, Vec<&'static str>)> {
    let service_log =
        "原因はサービスのログ（profile の logs\\banto-hub-service.log）で確かめられます。";
    match cause {
        FallbackCause::General => None,
        FallbackCause::ServiceLost => Some((
            "サービスが止まりました",
            vec![
                "接続していたサービスが止まりました。このアプリでは自動で Hub を起動しません。",
                "トレイの「再試行」で状態を確かめ直します。",
            ],
        )),
        FallbackCause::OperationStopped { left_tight: false } => Some((
            "運用モード（停止中）です",
            vec![
                "サービス（運用モード）が止まっています。profile のファイルは保護されたままなので、このアプリでは Hub を起動しません。",
                "運用を続けるときは、トレイの「サービスを開始」を押します。",
                "設定を変えるときは、トレイの「設定モードに戻す」を押します。ファイルの保護を外し、このアプリで Hub を起動します。",
                PAUSE_NOTE,
            ],
        )),
        FallbackCause::OperationStopped { left_tight: true } => Some((
            "運用モードのまま残っています",
            vec![
                "サービスが、profile のファイルの保護を外せないまま止まりました（終了コード 5）。このアプリでは Hub を起動しません。",
                "トレイの「設定モードに戻す」で保護を外します。それで戻らないときは、管理者の許可（ユーザーアカウント制御）を求めて直します。",
                "運用を続けるときは、トレイの「サービスを開始」を押します。",
                service_log,
                PAUSE_NOTE,
            ],
        )),
        FallbackCause::CannotOperate => Some((
            "運用モードにできませんでした",
            vec![
                "profile のファイルを保護できなかったため、運用モードに切り替えませんでした（終了コード 4）。ファイルは変わっておらず、設定モードのままです。",
                service_log,
                "トレイの「再試行」で、このアプリで Hub を起動し直します。",
            ],
        )),
        FallbackCause::ProfileUnusable => Some((
            "profile のフォルダを使えません",
            vec![
                "profile のフォルダがリンク（ジャンクション）になっているなど、安全に使えない状態です。このアプリでは Hub を起動しません。",
                "管理者に確かめてもらい、直した後にトレイの「再試行」を押します。",
            ],
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all_states() -> Vec<Option<ScmState>> {
        vec![
            None,
            Some(ScmState::NotInstalled),
            Some(ScmState::Stopped),
            Some(ScmState::StartPending),
            Some(ScmState::StopPending),
            Some(ScmState::Running),
            Some(ScmState::Other("Paused".into())),
        ]
    }

    fn all_modes() -> Vec<ProfileModeObservation> {
        vec![
            ProfileModeObservation::NotObserved,
            ProfileModeObservation::Unusable("reparse point".into()),
            ProfileModeObservation::Shape(ProfileShape::Absent),
            ProfileModeObservation::Shape(ProfileShape::Loose),
            ProfileModeObservation::Shape(ProfileShape::Tight),
            ProfileModeObservation::Shape(ProfileShape::Unreadable),
        ]
    }

    fn all_faults() -> Vec<Option<ServiceFault>> {
        vec![
            None,
            Some(ServiceFault::CannotOperate),
            Some(ServiceFault::LeftTight),
        ]
    }

    const TIGHT: ProfileModeObservation = ProfileModeObservation::Shape(ProfileShape::Tight);
    const LOOSE: ProfileModeObservation = ProfileModeObservation::Shape(ProfileShape::Loose);

    #[test]
    fn a_tight_profile_never_goes_desktop() {
        // どの SCM の状態・終了コードでも、締まった profile は TryDesktop にならない。
        for scm in all_states() {
            for fault in all_faults() {
                let plan = plan_startup(scm.as_ref(), &TIGHT, fault);
                assert_ne!(plan, StartupPlan::TryDesktop, "{scm:?} {fault:?}");
            }
        }
    }

    #[test]
    fn a_tight_profile_after_preshutdown_or_a_new_engine_is_root_blocked() {
        // PRESHUTDOWN の後（停止・終了コード 0）、新しい engine（過去を知らない）、
        // scan より前の 2・3（4・5 ではない）、照会の失敗のどれも「運用モード（停止中）」。
        for scm in [None, Some(ScmState::Stopped), Some(ScmState::NotInstalled)] {
            assert_eq!(
                plan_startup(scm.as_ref(), &TIGHT, None),
                StartupPlan::Fallback(FallbackCause::OperationStopped { left_tight: false }),
                "{scm:?}"
            );
        }
        // 5 で止まったなら「運用モードのまま残っています」。
        assert_eq!(
            plan_startup(
                Some(&ScmState::Stopped),
                &TIGHT,
                Some(ServiceFault::LeftTight)
            ),
            StartupPlan::Fallback(FallbackCause::OperationStopped { left_tight: true })
        );
    }

    #[test]
    fn a_loose_profile_goes_desktop_whatever_the_last_exit() {
        // 反証: 形が緩んでいれば（管理者が直した後など）、5 が残っていても Desktop。
        for mode in [
            LOOSE,
            ProfileModeObservation::Shape(ProfileShape::Absent),
            ProfileModeObservation::Shape(ProfileShape::Unreadable),
            ProfileModeObservation::NotObserved,
        ] {
            for fault in all_faults() {
                for scm in [None, Some(ScmState::Stopped), Some(ScmState::NotInstalled)] {
                    assert_eq!(
                        plan_startup(scm.as_ref(), &mode, fault),
                        StartupPlan::TryDesktop,
                        "{mode:?} {fault:?} {scm:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn the_mode_matters_only_for_the_desktop_outcomes() {
        for mode in all_modes() {
            assert_eq!(
                plan_startup(Some(&ScmState::Running), &mode, None),
                StartupPlan::ProbeService
            );
            for scm in [
                ScmState::StartPending,
                ScmState::StopPending,
                ScmState::Other("Paused".into()),
            ] {
                assert_eq!(
                    plan_startup(Some(&scm), &mode, None),
                    StartupPlan::Fallback(FallbackCause::General)
                );
            }
        }
        assert_eq!(
            plan_startup(
                Some(&ScmState::Stopped),
                &ProfileModeObservation::Unusable("link".into()),
                None
            ),
            StartupPlan::Fallback(FallbackCause::ProfileUnusable)
        );
    }

    #[test]
    fn switch_faults_pick_the_4_and_5_windows() {
        assert_eq!(
            switch_fault_cause(Some(ServiceFault::CannotOperate)),
            FallbackCause::CannotOperate
        );
        assert_eq!(
            switch_fault_cause(Some(ServiceFault::LeftTight)),
            FallbackCause::OperationStopped { left_tight: true }
        );
        assert_eq!(switch_fault_cause(None), FallbackCause::General);
    }

    #[test]
    fn lost_cause_reads_the_exit_only_once_stopped() {
        for fault in all_faults() {
            for scm in [
                Some(ScmState::StopPending),
                Some(ScmState::StartPending),
                Some(ScmState::NotInstalled),
                None,
            ] {
                assert_eq!(
                    lost_cause(scm.as_ref(), &TIGHT, fault),
                    FallbackCause::ServiceLost,
                    "{scm:?} {fault:?}"
                );
            }
        }
        let stopped = Some(&ScmState::Stopped);
        assert_eq!(
            lost_cause(stopped, &LOOSE, Some(ServiceFault::CannotOperate)),
            FallbackCause::CannotOperate
        );
        assert_eq!(
            lost_cause(stopped, &LOOSE, Some(ServiceFault::LeftTight)),
            FallbackCause::OperationStopped { left_tight: true }
        );
        assert_eq!(
            lost_cause(stopped, &TIGHT, None),
            FallbackCause::OperationStopped { left_tight: false }
        );
        assert_eq!(
            lost_cause(stopped, &LOOSE, None),
            FallbackCause::ServiceLost
        );
    }

    #[test]
    fn the_watch_never_starts_the_desktop_and_leaves_running_alone() {
        for scm in all_states() {
            for mode in all_modes() {
                for fault in all_faults() {
                    assert_eq!(
                        on_watch_tick(WatchedView::Desktop, scm.as_ref(), &mode, fault),
                        WatchStep::Stay
                    );
                }
            }
        }
        assert_eq!(
            on_watch_tick(WatchedView::Service, Some(&ScmState::Running), &TIGHT, None),
            WatchStep::Stay
        );
        assert_eq!(
            on_watch_tick(WatchedView::Service, None, &TIGHT, None),
            WatchStep::Stay
        );
    }

    #[test]
    fn stop_pending_then_stopped_with_5_reselects_the_window() {
        // Running → StopPending: 一般の「サービスが止まりました」。
        let step = on_watch_tick(
            WatchedView::Service,
            Some(&ScmState::StopPending),
            &ProfileModeObservation::NotObserved,
            None,
        );
        assert_eq!(step, WatchStep::ToFallback(FallbackCause::ServiceLost));
        // → Stopped・5: 選び直して「運用モードのまま残っています」（#690）。
        let step = on_watch_tick(
            WatchedView::Fallback(FallbackCause::ServiceLost),
            Some(&ScmState::Stopped),
            &TIGHT,
            Some(ServiceFault::LeftTight),
        );
        assert_eq!(
            step,
            WatchStep::Refresh(FallbackCause::OperationStopped { left_tight: true })
        );
        // → Stopped・4: 「運用モードにできませんでした」。
        assert_eq!(
            on_watch_tick(
                WatchedView::Fallback(FallbackCause::ServiceLost),
                Some(&ScmState::Stopped),
                &LOOSE,
                Some(ServiceFault::CannotOperate),
            ),
            WatchStep::Refresh(FallbackCause::CannotOperate)
        );
        // 反証: 利用者の操作の後の窓（一般）は、停止に達しても書き換えない。
        assert_eq!(
            on_watch_tick(
                WatchedView::Fallback(FallbackCause::General),
                Some(&ScmState::Stopped),
                &TIGHT,
                Some(ServiceFault::LeftTight),
            ),
            WatchStep::Stay
        );
    }

    #[test]
    fn mode_windows_fall_back_to_general_once_the_service_moves() {
        let cause = FallbackCause::OperationStopped { left_tight: true };
        assert_eq!(
            on_watch_tick(
                WatchedView::Fallback(cause),
                Some(&ScmState::StartPending),
                &ProfileModeObservation::NotObserved,
                None,
            ),
            WatchStep::Refresh(FallbackCause::General)
        );
        assert_eq!(
            on_watch_tick(
                WatchedView::Fallback(cause),
                Some(&ScmState::Stopped),
                &TIGHT,
                None
            ),
            WatchStep::Stay
        );
    }

    #[test]
    fn relax_only_0_decides_and_5_or_failure_elevates() {
        assert_eq!(
            after_relax(&RelaxOnlyOutcome::Done(ServiceEnd::Clean)),
            AfterRelax::Decide
        );
        assert_eq!(
            after_relax(&RelaxOnlyOutcome::Done(ServiceEnd::LeftOperation)),
            AfterRelax::Elevate { left_tight: true }
        );
        for outcome in [
            RelaxOnlyOutcome::Done(ServiceEnd::CannotOperate),
            RelaxOnlyOutcome::Done(ServiceEnd::Other {
                win32: 1066,
                specific: 3,
            }),
            RelaxOnlyOutcome::NotInstalled,
            RelaxOnlyOutcome::Refused("拒否".into()),
            RelaxOnlyOutcome::Unexpected(ScmState::Running),
            RelaxOnlyOutcome::Stalled,
            RelaxOnlyOutcome::TimedOut,
        ] {
            assert_eq!(
                after_relax(&outcome),
                AfterRelax::Elevate { left_tight: false },
                "{outcome:?}"
            );
        }
        assert!(matches!(
            after_relax(&RelaxOnlyOutcome::NotStopped(ScmState::Running)),
            AfterRelax::Running(text) if text.contains("Running")
        ));
    }

    #[test]
    fn only_tight_windows_offer_restore_and_keys_are_stable() {
        assert!(FallbackCause::OperationStopped { left_tight: true }.offers_restore());
        assert!(FallbackCause::OperationStopped { left_tight: false }.offers_restore());
        for cause in [
            FallbackCause::General,
            FallbackCause::ServiceLost,
            FallbackCause::ProfileUnusable,
            FallbackCause::CannotOperate,
        ] {
            assert!(!cause.offers_restore(), "{cause:?}");
        }
        assert_eq!(
            FallbackCause::OperationStopped { left_tight: true }.key(),
            Some("leftTight")
        );
        assert_eq!(FallbackCause::CannotOperate.key(), Some("cannotOperate"));
        assert_eq!(FallbackCause::General.key(), None);
    }

    #[test]
    fn pages_name_the_way_back_and_the_pause() {
        let (title, lines) =
            cause_page(FallbackCause::OperationStopped { left_tight: true }).expect("窓");
        assert_eq!(title, "運用モードのまま残っています");
        assert!(lines.iter().any(|l| l.contains("設定モードに戻す")));
        assert!(lines.iter().any(|l| l.contains("終了コード 5")));
        assert!(lines.contains(&PAUSE_NOTE));
        let (_, lines) = cause_page(FallbackCause::CannotOperate).expect("窓");
        assert!(lines.iter().any(|l| l.contains("設定モードのまま")));
        assert!(!lines.iter().any(|l| l.contains("設定モードに戻す")));
        assert!(cause_page(FallbackCause::General).is_none());
    }

    #[test]
    fn switch_phases_have_progress_texts_while_moving() {
        assert!(switch_phase_message(&SwitchPhase::ServiceStarting)
            .is_some_and(|t| t.contains("ファイルの権限を設定しています")));
        assert!(switch_phase_message(&SwitchPhase::AwaitingServiceRelease)
            .is_some_and(|t| t.contains("元に戻しています")));
        assert!(switch_phase_message(&SwitchPhase::Idle).is_none());
    }
}
