//! T17-3（docs/banto-hub-t17-design.md §3「T17-3」・§4「T16-2 への引き渡し
//! 契約」）: 設計 §4 に記述だけがあった [`HubHealthProbe`] を実装するモジュール。
//!
//! fallback UI（T16-2、desktop-plan §9.9）が「別の Banto Hub が使用中、
//! または状態を確認できません」を判定するための health/所有権確認 - SCM の
//! `Running`/`Stopped`（`crate::service_manager::ServiceManager`）だけでは
//! 「起動したがまだ健全でない」「別 profile/version が別ポートで応答して
//! いる」を区別できないため、この trait が別軸の確認を提供する。
//!
//! ## この T17-3 で行ったこと・行っていないこと
//!
//! - [`HubHealthProbe`] trait・[`HealthOutcome`]・[`ProbeError`]は設計 §4 の
//!   契約どおりホスト非依存 - Windows API に触れないので非 Windows（この
//!   ワークスペースの CI）でも単体テストできる。
//! - [`MockHubHealthProbe`]（テスト用、常に利用可能）を用意した - 固定の
//!   outcome を返す既定値と、`push_sequence`で予約した出力を先頭から順に
//!   1回ずつ消費する queue の2段構成（[`crate::service_manager::MockServiceManager::inject_error`]
//!   の一時失敗フックと同じ発想）。
//! - **実 HTTP probe**は T17-3 時点では入れていなかったが、T16-2
//!   （docs/banto-hub-t16-design.md §3「T16-2」）で
//!   [`crate::http_hub_health::HttpHubHealthProbe`]として追加した -
//!   [`crate::host_switch`]はこの trait だけに依存していたため、
//!   [`crate::host_switch::HostSwitchEngine`]側の変更は不要だった
//!   （予告どおり）。

use std::collections::VecDeque;
use std::sync::Mutex;

use thiserror::Error;

/// Hub の health（`/api/v1/openapi.json`相当のヘルスチェック応答）を確認する
/// ホスト非依存の trait（設計 §4 の契約そのもの）。
///
/// [`crate::host_switch::HostSwitchEngine`]・T16-2 fallback UI はこの trait
/// だけを消費し、実際の HTTP クライアントや named mutex API を直接呼ばない。
pub trait HubHealthProbe {
    /// `expected_profile`/`expected_port`は「これから所有権を確認したい
    /// Hub インスタンス」の期待値 - 応答した Hub がこれと食い違う場合は
    /// [`HealthOutcome::WrongProfileOrVersion`]を返す（別 profile / 別
    /// version の Hub が同じ操作対象だと誤認しないため）。
    fn probe(
        &self,
        expected_profile: &str,
        expected_port: u16,
    ) -> Result<HealthOutcome, ProbeError>;
}

/// [`HubHealthProbe::probe`]の判定結果（desktop-plan §9.9 の fallback UI
/// 文言「サービス: 実行中 / 管理画面: 応答なし」「mutex: 所有者不明」等は
/// この enum の値そのもの - 具体的な文言は消費する側が持つ）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HealthOutcome {
    /// 期待する profile/version の Hub が健全に応答した。
    Healthy { version: String },
    /// 応答はあったが profile または version が期待と食い違う - 別ホストの
    /// Hub を誤って所有権確認済みと判定しないための安全側の分類（desktop-plan
    /// §9.9「別 port や別ホストへ自動で逃がさない」）。
    WrongProfileOrVersion,
    /// health は応答するが所有者（mutex owner）が特定できない -
    /// `profile.lock`の診断情報（[`crate::profile_lock::ProfileOwnerInfo`]）が
    /// 読めない、または SCM 状態と矛盾する場合。
    MutexOwnerUnknown,
    /// 期待するポートで別プロセスが別プロトコルで listen している等、
    /// port が競合している。
    PortConflict,
    /// health が応答しない（`Running`なのに応答なし、接続タイムアウト等）。
    Unreachable,
}

/// [`HubHealthProbe::probe`]自体の実行に失敗したことを表すエラー - 上記
/// [`HealthOutcome::Unreachable`]（「health に到達したが応答が来ない」）とは
/// 区別し、probe の実装そのもの（DNS 解決失敗・不正な設定等）が壊れている
/// 場合に使う。
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ProbeError {
    #[error("banto-hub: health probe の実行に失敗しました: {0}")]
    Other(String),
}

/// [`HubHealthProbe`]のインメモリモック実装 - 単体テスト用
/// （このファイル末尾の`tests`モジュール、および[`crate::host_switch`]の
/// テストが消費する）。
///
/// 固定の既定 outcome（[`Self::set_outcome`]で変更可）に加え、
/// [`Self::push_sequence`]で予約した outcome を先頭から1回ずつ消費する -
/// 「最初は Unreachable、次は Healthy」のような段階的シナリオを表現できる。
pub struct MockHubHealthProbe {
    state: Mutex<MockProbeState>,
}

struct MockProbeState {
    default_outcome: Result<HealthOutcome, ProbeError>,
    queued_outcomes: VecDeque<Result<HealthOutcome, ProbeError>>,
    /// 直前に`probe`へ渡された引数 - テストの呼び出しアサーション用。
    last_call: Option<(String, u16)>,
    call_count: u32,
}

impl MockHubHealthProbe {
    /// `default_outcome`を既定値として返し続けるモックを作る。
    pub fn new(default_outcome: HealthOutcome) -> Self {
        Self {
            state: Mutex::new(MockProbeState {
                default_outcome: Ok(default_outcome),
                queued_outcomes: VecDeque::new(),
                last_call: None,
                call_count: 0,
            }),
        }
    }

    /// [`HealthOutcome::Unreachable`]を既定値とするモック - 「まだ健全になって
    /// いない/停止済みで health が消失した」を表す初期値として使いやすい。
    pub fn unreachable() -> Self {
        Self::new(HealthOutcome::Unreachable)
    }

    /// 既定値を変更する（以降、queue が空になった呼び出しはこれを返す）。
    pub fn set_outcome(&self, outcome: HealthOutcome) {
        self.state
            .lock()
            .expect("mock probe mutex poisoned")
            .default_outcome = Ok(outcome);
    }

    /// 既定値をエラーに変更する。
    pub fn set_error(&self, err: ProbeError) {
        self.state
            .lock()
            .expect("mock probe mutex poisoned")
            .default_outcome = Err(err);
    }

    /// `outcomes`を先頭から1回ずつ消費する queue の末尾へ追加する -
    /// 呼び出し順に段階的な outcome を再現したいテストで使う。
    pub fn push_sequence(&self, outcomes: impl IntoIterator<Item = HealthOutcome>) {
        let mut state = self.state.lock().expect("mock probe mutex poisoned");
        state.queued_outcomes.extend(outcomes.into_iter().map(Ok));
    }

    /// 直前の`probe`呼び出しに渡された`(expected_profile, expected_port)`。
    pub fn last_call(&self) -> Option<(String, u16)> {
        self.state
            .lock()
            .expect("mock probe mutex poisoned")
            .last_call
            .clone()
    }

    /// `probe`が呼ばれた合計回数（タイムアウト経路のテストで「何回ポーリング
    /// したか」を確認するために使う）。
    pub fn call_count(&self) -> u32 {
        self.state
            .lock()
            .expect("mock probe mutex poisoned")
            .call_count
    }
}

impl HubHealthProbe for MockHubHealthProbe {
    fn probe(
        &self,
        expected_profile: &str,
        expected_port: u16,
    ) -> Result<HealthOutcome, ProbeError> {
        let mut state = self.state.lock().expect("mock probe mutex poisoned");
        state.last_call = Some((expected_profile.to_string(), expected_port));
        state.call_count += 1;
        if let Some(next) = state.queued_outcomes.pop_front() {
            return next;
        }
        state.default_outcome.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_outcome_is_returned_repeatedly() {
        let probe = MockHubHealthProbe::new(HealthOutcome::Healthy {
            version: "1.0.0".to_string(),
        });
        for _ in 0..3 {
            assert_eq!(
                probe.probe("default", 8722),
                Ok(HealthOutcome::Healthy {
                    version: "1.0.0".to_string()
                })
            );
        }
        assert_eq!(probe.call_count(), 3);
    }

    #[test]
    fn queued_sequence_is_consumed_in_order_then_falls_back_to_default() {
        let probe = MockHubHealthProbe::unreachable();
        probe.push_sequence([
            HealthOutcome::MutexOwnerUnknown,
            HealthOutcome::PortConflict,
        ]);

        assert_eq!(
            probe.probe("default", 8722),
            Ok(HealthOutcome::MutexOwnerUnknown)
        );
        assert_eq!(
            probe.probe("default", 8722),
            Ok(HealthOutcome::PortConflict)
        );
        // queue が空になったら既定値（Unreachable）へ戻る。
        assert_eq!(probe.probe("default", 8722), Ok(HealthOutcome::Unreachable));
    }

    #[test]
    fn last_call_records_expected_profile_and_port() {
        let probe = MockHubHealthProbe::unreachable();
        assert_eq!(probe.last_call(), None);
        probe.probe("line-1", 9000).ok();
        assert_eq!(probe.last_call(), Some(("line-1".to_string(), 9000)));
    }

    #[test]
    fn set_error_makes_probe_return_err() {
        let probe = MockHubHealthProbe::unreachable();
        probe.set_error(ProbeError::Other("dns error".to_string()));
        assert!(matches!(
            probe.probe("default", 8722),
            Err(ProbeError::Other(_))
        ));
    }
}
