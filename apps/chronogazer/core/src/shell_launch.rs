//! シェル（デスクトップの `src-tauri`）の起動判定（#392 段階 B の B2-4a、
//! `docs/chronogazer-service-design.md` §3.2・2026-10-14 オーナー決定 O1・O8）。
//!
//! シェルは起動のたびに「サービスへつなぐ（navigate）」「プロセスの中でエンジンを
//! 動かす（デスクトップ host）」「どちらもせず状態を見せる（fallback）」の 3 つから
//! 1 つを選ぶ。このモジュールは**その判断だけ**を純関数で持つ（Tauri にも SCM にも
//! ネットワークにも触れない）。観測（SCM の照会・`GET /api/host` の probe）は
//! 呼び出し側が行い、結果を [`ScmObservation`]・[`ServiceHealth`] で渡す。banto-hub の
//! `decide_startup`（`apps/banto-hub/src-tauri/src/lib.rs`）は判断と I/O が 1 つの
//! 関数に混ざっていたので、ChronoGazer では分けた（判断を表で総当たりにテストする
//! ため。実装チェックリスト §5）。
//!
//! # 判定表（[`decide`]）
//!
//! | SCM の観測                                  | probe            | 判定                     |
//! | ------------------------------------------- | ---------------- | ------------------------ |
//! | Windows でない（[`ScmObservation::NotWindows`]） | しない      | Desktop                  |
//! | 照会に失敗（[`ScmObservation::QueryFailed`]）   | しない      | Desktop（O1）            |
//! | `NotInstalled`・`Stopped`                   | しない           | Desktop                  |
//! | `StartPending`・`StopPending`・`Other(_)`   | しない           | Fallback                 |
//! | `Running`                                   | Healthy          | Service（その port へ）  |
//! | `Running`                                   | それ以外・無し   | Fallback                 |
//!
//! **約束（不変条件）**: SCM が遷移中（`StartPending`・`StopPending`・`Other`）または
//! `Running` なら、probe の結果が何であっても**決して Desktop にしない**（サービスが
//! 同じ DB を開いている・開こうとしている間にプロセスの中でエンジンを起動しない。
//! A1 の排他は最後の防衛線で、ここが最初の防衛線）。照会に失敗したときだけは
//! Desktop に倒す（O1: サービスが実際に動いていれば A1 がデスクトップの起動を
//! 止め、既存のエラーの窓が出る）。
//!
//! probe を打つかどうかは [`needs_probe`] の 1 つの述語で決め、[`decide`] も同じ
//! 述語で probe の結果を見るかどうかを決める（「起こす条件」と「使う条件」を
//! 1 つにする。実装チェックリスト §5）。probe しない状態で渡された結果は無視する。
//!
//! # Healthy の意味（O8）
//!
//! [`ServiceHealth::Healthy`] は、`run\endpoint.json` で見つけた port の
//! `GET /api/host` が `product == "chronogazer"`・`hostKind == "service"`・`scope` が
//! endpoint の値と（ルートが既にあれば）自分の DB の値の両方に一致したときだけ
//! （判定は `crate::service_probe`）。それ以外の理由は Fallback の窓に出すために
//! 種類ごとに分けて持つ。

use banto_safe_root::ModeObservation;
use banto_service_control::{ScmState, ServiceManager};

/// SCM 上の ChronoGazer のサービス名（決定 7。B3-2 のサービスの登録と同じ値）。
pub const SERVICE_NAME: &str = "ChronoGazer";
/// SCM 上の表示名（決定 7）。
pub const SERVICE_DISPLAY_NAME: &str = "ChronoGazer 記録エンジン";
/// SCM 上の説明文（#392 B3-2b。`install` の新規登録で設定する）。
pub const SERVICE_DESCRIPTION: &str =
    "ChronoGazer の記録エンジン（PLC のデータ収集と保存）を常駐実行します。データ: %ProgramData%\\ChronoGazer。";
/// SCM がサービスの開始で `chronogazer-server.exe` に渡す引数（#392 B3-2b）。
pub const RUN_SERVICE_ARG: &str = "run-service";

/// ChronoGazer のサービスの登録内容（#392 B3-2b。`chronogazer-server.exe install` が
/// `banto_service_control::install_service` に渡す）。名前・表示名はシェルの照会
/// （B2-4c）と同じ定数。LocalSystem・OwnProcess・手動開始（OS の再起動だけでは
/// 始まらない。自動開始は B3-3 の elev）・OS シャットダウンの待ち時間 30 秒
/// （PRESHUTDOWN。最終 flush の予算はその内側、`crate::server_service`）。
pub fn chronogazer_service_spec(
    executable: std::path::PathBuf,
) -> banto_service_control::ServiceSpec {
    banto_service_control::ServiceSpec {
        name: SERVICE_NAME,
        display_name: SERVICE_DISPLAY_NAME,
        description: Some(SERVICE_DESCRIPTION),
        executable,
        launch_arguments: vec![std::ffi::OsString::from(RUN_SERVICE_ARG)],
        start_kind: banto_service_control::ServiceStartKind::OnDemand,
        preshutdown_timeout: banto_service_control::PRESHUTDOWN_TIMEOUT,
    }
}

/// SCM の観測の結果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScmObservation {
    /// Windows 以外（SCM が無い）。
    NotWindows,
    /// 照会に失敗した（理由。秘密は含まない - `ServiceManagerError` の文言）。
    QueryFailed(String),
    /// 照会できた状態。
    State(ScmState),
}

/// サービスの health probe の結果（`crate::service_probe` が作る）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServiceHealth {
    /// つないでよい（O8 の条件をすべて満たした）。
    Healthy { port: u16, version: String },
    /// `run\endpoint.json` が無い・壊れている・書いたプロセスが生きていない。
    EndpointMissing,
    /// endpoint の port に TCP でつながらない（拒否・時間切れ）。
    Unreachable { port: u16 },
    /// つながったが ChronoGazer の `/api/host` の応答ではない（別製品・別の
    /// HTTP サーバー・HTTP ではない）。
    NotChronoGazer { port: u16 },
    /// ChronoGazer だが host の種類がサービスではない（`kind` は応答の値）。
    WrongHostKind { port: u16, kind: String },
    /// ChronoGazer のサービスだが、別の DB（endpoint の `scope` または自分の DB の
    /// `scope` と食い違う）。
    WrongScope { port: u16 },
    /// つながった後の読み書きに失敗した（応答が返ってこない・途中で切れた）。
    ProbeFailed(String),
}

/// Fallback の窓に出す情報。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FallbackInfo {
    /// そのときの SCM の状態（Fallback は SCM の状態が分かったときだけ）。
    pub scm: ScmState,
    /// probe の結果（probe しない状態・probe の結果が無いときは `None`）。
    pub health: Option<ServiceHealth>,
}

/// 起動判定の結果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LaunchDecision {
    /// サービスへつなぐ（`http://127.0.0.1:<port>/` へ navigate）。
    Service { port: u16 },
    /// プロセスの中でエンジンを動かす。`scm` は判定の根拠になった SCM の状態
    /// （Windows でない・照会に失敗したときは `None`）。
    Desktop { scm: Option<ScmState> },
    /// どちらもせず、状態と対処を見せる。
    Fallback(FallbackInfo),
    /// Desktop にするはずだったが、データのルートがそれを許さない（#623 PR-3、設計 §3.6）:
    /// ルートが**運用モードの形**（締まったまま。サービスが落ちた・PRESHUTDOWN で止まった）か、
    /// ルートをモードの観測に使えない（reparse point・フォルダでない）。どちらも Desktop に
    /// しない（締まった木でエンジンを起動して分かりにくく失敗するのを防ぐ）。文言・ボタン
    /// （[運用を再開]・[設定モードに切り替える]）は PR-4。
    RootBlocked { scm: ScmObservation, why: RootBlock },
}

/// [`LaunchDecision::RootBlocked`] の理由。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RootBlock {
    /// ルートの DACL が運用モードの形（[`ModeObservation::Operation`]）。
    OperationMode,
    /// ルートをモードの観測に使えない（`observe_root` が断った理由の文言）。
    Unusable(String),
}

/// データのルートのモードの観測（デスクトップの利用者の権限で、ルートの DACL を読むだけ。
/// `banto_safe_root::observe_root` → [`banto_safe_root::classify_mode`]。#623 PR-3、設計 §2.10）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RootModeObservation {
    /// 読めた（無い・開けないも含む。[`ModeObservation`]）。
    Mode(ModeObservation),
    /// ルートが reparse point・フォルダでない・読む途中の失敗（理由の文言）。
    Unusable(String),
    /// 観測していない（Windows でない）。
    NotObserved,
}

impl RootModeObservation {
    /// Desktop を断る理由（断らないなら `None`）。**断るのは運用モードの形と、ルートを
    /// 観測に使えないときだけ**（設定モード・無い・どちらの形でもない・自分の ACE が無いは
    /// Desktop へ進み、`check_writable_dir` が最後の防衛線。Fable-2 A-2）。
    pub fn blocks_desktop(&self) -> Option<RootBlock> {
        match self {
            Self::Mode(ModeObservation::Operation) => Some(RootBlock::OperationMode),
            Self::Unusable(reason) => Some(RootBlock::Unusable(reason.clone())),
            Self::Mode(_) | Self::NotObserved => None,
        }
    }
}

/// ルートのモードを観測する（Windows: ルートを読むだけで開き、今の利用者の SID で見分ける。
/// 非 Windows: [`RootModeObservation::NotObserved`]）。ルートを作らない・書かない。
pub fn observe_root_mode(root: &std::path::Path) -> RootModeObservation {
    #[cfg(windows)]
    {
        let me = match banto_safe_root::acl::current_user_sid() {
            Ok(me) => me,
            Err(err) => return RootModeObservation::Unusable(err.to_string()),
        };
        match banto_safe_root::observe_root(root) {
            Ok(observed) => {
                RootModeObservation::Mode(banto_safe_root::classify_mode(&observed, &me))
            }
            Err(err) => RootModeObservation::Unusable(err.to_string()),
        }
    }
    #[cfg(not(windows))]
    {
        let _ = root;
        RootModeObservation::NotObserved
    }
}

/// probe を打つべきか（`Running` のときだけ）。[`decide`] も同じ述語で probe の
/// 結果を見るかどうかを決める。
pub fn needs_probe(scm: &ScmObservation) -> bool {
    matches!(scm, ScmObservation::State(ScmState::Running))
}

/// SCM の照会の結果を [`ScmObservation`] に直す（B2-4c。シェルは本番で
/// `WindowsServiceManager`、テストは `MockServiceManager` を渡す）。照会の失敗は
/// `ServiceManagerError` の文言（中立、秘密を含まない - O9）で [`ScmObservation::QueryFailed`]。
/// 未登録はエラーではなく `ScmState::NotInstalled`（`ServiceManager::query_status` の契約）。
pub fn observe_scm(manager: &impl ServiceManager) -> ScmObservation {
    match manager.query_status() {
        Ok(summary) => ScmObservation::State(summary.state),
        Err(err) => ScmObservation::QueryFailed(err.to_string()),
    }
}

/// 起動判定（モジュール doc の判定表）。`health` は [`needs_probe`] が真のときだけ
/// 見る（偽のときに渡された値は無視する）。`mode` はデータのルートのモードの観測で、
/// **Desktop になる組合せだけ**に効く（[`RootModeObservation::blocks_desktop`] なら
/// [`LaunchDecision::RootBlocked`]。Service・Fallback には影響しない。#623 PR-3）。
/// 約束に 1 つ足す: **ルートが運用モードの形なら決して Desktop にしない**（O1 の
/// 「照会の失敗は Desktop」もその中に限る）。
pub fn decide(
    scm: &ScmObservation,
    health: Option<&ServiceHealth>,
    mode: &RootModeObservation,
) -> LaunchDecision {
    match decide_without_mode(scm, health) {
        LaunchDecision::Desktop { .. } if mode.blocks_desktop().is_some() => {
            LaunchDecision::RootBlocked {
                scm: scm.clone(),
                why: mode.blocks_desktop().expect("上で見た"),
            }
        }
        other => other,
    }
}

/// モードを見ない判定表（B2-4a のまま）。
fn decide_without_mode(scm: &ScmObservation, health: Option<&ServiceHealth>) -> LaunchDecision {
    let state = match scm {
        ScmObservation::NotWindows | ScmObservation::QueryFailed(_) => {
            return LaunchDecision::Desktop { scm: None };
        }
        ScmObservation::State(state) => state,
    };
    if needs_probe(scm) {
        return match health {
            Some(ServiceHealth::Healthy { port, .. }) => LaunchDecision::Service { port: *port },
            other => LaunchDecision::Fallback(FallbackInfo {
                scm: state.clone(),
                health: other.cloned(),
            }),
        };
    }
    match state {
        ScmState::NotInstalled | ScmState::Stopped => LaunchDecision::Desktop {
            scm: Some(state.clone()),
        },
        // 遷移中・知らない状態はローカルで起動しない（モジュール doc の約束）。
        ScmState::StartPending | ScmState::StopPending | ScmState::Other(_) => {
            LaunchDecision::Fallback(FallbackInfo {
                scm: state.clone(),
                health: None,
            })
        }
        ScmState::Running => unreachable!("Running は needs_probe の側で扱う"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all_states() -> Vec<ScmState> {
        vec![
            ScmState::NotInstalled,
            ScmState::Stopped,
            ScmState::StartPending,
            ScmState::StopPending,
            ScmState::Running,
            ScmState::Other("Paused".to_string()),
        ]
    }

    fn all_observations() -> Vec<ScmObservation> {
        let mut all = vec![
            ScmObservation::NotWindows,
            ScmObservation::QueryFailed("サービス操作に失敗しました: os error 5".to_string()),
        ];
        all.extend(all_states().into_iter().map(ScmObservation::State));
        all
    }

    fn all_health() -> Vec<Option<ServiceHealth>> {
        vec![
            None,
            Some(ServiceHealth::Healthy {
                port: 8721,
                version: "0.1.0".to_string(),
            }),
            Some(ServiceHealth::EndpointMissing),
            Some(ServiceHealth::Unreachable { port: 8721 }),
            Some(ServiceHealth::NotChronoGazer { port: 8721 }),
            Some(ServiceHealth::WrongHostKind {
                port: 8721,
                kind: "server".to_string(),
            }),
            Some(ServiceHealth::WrongScope { port: 8721 }),
            Some(ServiceHealth::ProbeFailed("timed out".to_string())),
        ]
    }

    fn no_mode() -> RootModeObservation {
        RootModeObservation::NotObserved
    }

    fn all_modes() -> Vec<RootModeObservation> {
        vec![
            RootModeObservation::NotObserved,
            RootModeObservation::Mode(ModeObservation::Setting),
            RootModeObservation::Mode(ModeObservation::Operation),
            RootModeObservation::Mode(ModeObservation::NoAccessForMe),
            RootModeObservation::Mode(ModeObservation::Unexpected(
                banto_safe_root::AclMismatch::NotProtected,
            )),
            RootModeObservation::Mode(ModeObservation::Absent),
            RootModeObservation::Unusable("リンク".to_string()),
        ]
    }

    /// #623 PR-3: モードは Desktop になる組合せにだけ効く。運用モードの形・観測に使えない
    /// ルートは `RootBlocked`（決して Desktop にしない）、それ以外は何も変えない。
    #[test]
    fn mode_blocks_only_the_desktop_outcomes() {
        for scm in all_observations() {
            for health in all_health() {
                let base = decide(&scm, health.as_ref(), &no_mode());
                for mode in all_modes() {
                    let got = decide(&scm, health.as_ref(), &mode);
                    match mode.blocks_desktop() {
                        Some(why) if matches!(base, LaunchDecision::Desktop { .. }) => {
                            assert_eq!(
                                got,
                                LaunchDecision::RootBlocked {
                                    scm: scm.clone(),
                                    why
                                },
                                "scm={scm:?} health={health:?} mode={mode:?}"
                            );
                        }
                        _ => assert_eq!(got, base, "scm={scm:?} health={health:?} mode={mode:?}"),
                    }
                    assert!(
                        !(mode == RootModeObservation::Mode(ModeObservation::Operation)
                            && matches!(got, LaunchDecision::Desktop { .. })),
                        "運用モードの形で Desktop にした: scm={scm:?} health={health:?}"
                    );
                }
            }
        }
    }

    fn is_transitional_or_running(scm: &ScmObservation) -> bool {
        matches!(
            scm,
            ScmObservation::State(
                ScmState::StartPending
                    | ScmState::StopPending
                    | ScmState::Running
                    | ScmState::Other(_)
            )
        )
    }

    /// 判定表の全組合せ（SCM の観測 8 × probe の結果 8）を期待値と突き合わせる。
    #[test]
    fn decision_table_covers_every_combination() {
        for scm in all_observations() {
            for health in all_health() {
                let got = decide(&scm, health.as_ref(), &no_mode());
                let expected = match (&scm, &health) {
                    (ScmObservation::NotWindows | ScmObservation::QueryFailed(_), _) => {
                        LaunchDecision::Desktop { scm: None }
                    }
                    (
                        ScmObservation::State(s @ (ScmState::NotInstalled | ScmState::Stopped)),
                        _,
                    ) => LaunchDecision::Desktop {
                        scm: Some(s.clone()),
                    },
                    (
                        ScmObservation::State(ScmState::Running),
                        Some(ServiceHealth::Healthy { port, .. }),
                    ) => LaunchDecision::Service { port: *port },
                    (ScmObservation::State(ScmState::Running), other) => {
                        LaunchDecision::Fallback(FallbackInfo {
                            scm: ScmState::Running,
                            health: other.clone(),
                        })
                    }
                    (ScmObservation::State(s), _) => LaunchDecision::Fallback(FallbackInfo {
                        scm: s.clone(),
                        health: None,
                    }),
                };
                assert_eq!(got, expected, "scm={scm:?} health={health:?}");
            }
        }
    }

    /// 約束: 遷移中・`Running` なら probe の結果が何であっても Desktop にしない。
    #[test]
    fn transitional_or_running_never_goes_desktop() {
        for scm in all_observations()
            .into_iter()
            .filter(is_transitional_or_running)
        {
            for health in all_health() {
                let got = decide(&scm, health.as_ref(), &no_mode());
                assert!(
                    !matches!(got, LaunchDecision::Desktop { .. }),
                    "scm={scm:?} health={health:?} -> {got:?}"
                );
            }
        }
    }

    /// Service になるのは `Running` かつ Healthy のときだけ。
    #[test]
    fn service_only_when_running_and_healthy() {
        for scm in all_observations() {
            for health in all_health() {
                let got = decide(&scm, health.as_ref(), &no_mode());
                let running_healthy = scm == ScmObservation::State(ScmState::Running)
                    && matches!(health, Some(ServiceHealth::Healthy { .. }));
                assert_eq!(
                    matches!(got, LaunchDecision::Service { .. }),
                    running_healthy,
                    "scm={scm:?} health={health:?} -> {got:?}"
                );
            }
        }
    }

    /// probe するのは `Running` のときだけで、probe しない状態では渡された結果に
    /// 判定が左右されない（「起こす条件」と「使う条件」が同じ述語）。
    #[test]
    fn probe_is_needed_only_when_running_and_ignored_otherwise() {
        for scm in all_observations() {
            assert_eq!(
                needs_probe(&scm),
                scm == ScmObservation::State(ScmState::Running),
                "{scm:?}"
            );
            if !needs_probe(&scm) {
                let baseline = decide(&scm, None, &no_mode());
                for health in all_health() {
                    assert_eq!(
                        decide(&scm, health.as_ref(), &no_mode()),
                        baseline,
                        "{scm:?} {health:?}"
                    );
                }
            }
        }
    }

    /// O1: 照会の失敗は Desktop（A1 が最後の防衛線）。
    #[test]
    fn query_failure_falls_back_to_desktop() {
        assert_eq!(
            decide(
                &ScmObservation::QueryFailed("x".to_string()),
                None,
                &no_mode()
            ),
            LaunchDecision::Desktop { scm: None }
        );
    }

    /// 照会の結果の写し方: 状態はそのまま、失敗は中立な文言で `QueryFailed`。
    #[test]
    fn observe_scm_maps_status_and_errors() {
        use banto_service_control::{MockServiceManager, ServiceManagerError};
        assert_eq!(
            observe_scm(&MockServiceManager::not_installed()),
            ScmObservation::State(ScmState::NotInstalled)
        );
        let mock = MockServiceManager::new();
        assert_eq!(observe_scm(&mock), ScmObservation::State(ScmState::Stopped));
        mock.inject_error(ServiceManagerError::AccessDenied);
        assert_eq!(
            observe_scm(&mock),
            ScmObservation::QueryFailed("サービス操作の権限がありません".to_string())
        );
    }

    #[test]
    fn names_match_decision_7() {
        assert_eq!(SERVICE_NAME, "ChronoGazer");
        assert_eq!(SERVICE_DISPLAY_NAME, "ChronoGazer 記録エンジン");
    }

    /// #392 B3-2b: 登録内容の golden（名前・表示名・説明文・起動引数・手動開始・
    /// PRESHUTDOWN 30 秒）。
    #[test]
    fn service_spec_golden() {
        use banto_service_control::{ServiceSpec, ServiceStartKind};
        use std::path::PathBuf;
        use std::time::Duration;

        let exe = PathBuf::from(r"C:\Program Files\ChronoGazer\chronogazer-server.exe");
        assert_eq!(
            chronogazer_service_spec(exe.clone()),
            ServiceSpec {
                name: "ChronoGazer",
                display_name: "ChronoGazer 記録エンジン",
                description: Some(
                    "ChronoGazer の記録エンジン（PLC のデータ収集と保存）を常駐実行します。データ: %ProgramData%\\ChronoGazer。"
                ),
                executable: exe,
                launch_arguments: vec![std::ffi::OsString::from("run-service")],
                start_kind: ServiceStartKind::OnDemand,
                preshutdown_timeout: Duration::from_secs(30),
            }
        );
    }
}
