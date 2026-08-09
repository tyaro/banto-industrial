//! テスト出力 (T15-3、docs/banto-hub-desktop-plan.md §6.3・T15)。
//!
//! [`TestOutputControl`] は「今の収集 run に対してだけテスト出力を送るか」
//! を持つ、[`crate::write_control::WriteControl`] と同型のロックフリーな
//! 薄いフラグ保持者。書き込み受付（`WriteControl`）との違いは、このフラグ
//! が「有効/無効」だけでなく「**どの run_id に対して**有効か」も保持する
//! 点にある — テスト出力は明示的に有効化した現在の run コンテキストだけに
//! 限定する（設計 §6.3「現在の run コンテキストのみ」）ため、run_id の一致
//! までを [`TestOutputControl::is_active_for`] が1箇所で判定する。
//!
//! ## 一切永続化しない
//!
//! `WriteControl` は「起動時は常に disabled」だが *表示専用の* 永続値
//! （`was_enabled_before_restart`）を DB に持つ。テスト出力にはそれすら無い。
//! 停止・終了・モード切替・サービス再起動のいずれでも必ず無効へ戻るのが
//! 要件そのものであり（設計 §6.3「停止／終了／切替／サービス再起動後に
//! 必ず無効へ戻る」）、次回のために憶えておく値が存在しない。DB テーブルも
//! 持たない。
//!
//! ## Pure, sync, DB-free
//!
//! `AtomicBool` + `AtomicU64` のみで構成する（`write_control.rs`の
//! モジュール doc comment「Pure, sync, DB-free」と同じ設計）。呼び出し元
//! （`crate::controller::CollectionController`・`crate::rest`の管理 REST
//! ハンドラ・`crate::mqtt`/`crate::grpc`の評価/配信経路）はこの構造体を
//! `Arc`で共有するだけで、DB アクセスや非同期処理を一切要求しない。

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use crate::controller::RunId;

/// `armed_run_id`の「未設定」を表すセンチネル。
/// `CollectionController::run_seq`は1から採番される
/// （`CollectionController::start_locked`参照）ため、実際の run_id として
/// 0が現れることはない。
const NO_RUN: u64 = 0;

/// [`TestOutputControl::status`]が返すスナップショット - REST の
/// `GET /api/v1/status`の`test_output: { enabled, run_id }`
/// （`crate::rest`）はこれをそのまま JSON へ写す。
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct TestOutputStatus {
    pub enabled: bool,
    pub run_id: Option<RunId>,
}

/// テスト出力の実行時フラグ。構築時は常に無効
/// （[`TestOutputControl::new`]）- `WriteControl::new`と同じく、有効化は
/// 呼び出し元（`crate::rest`の`POST /api/test-output/enable`）の明示操作を
/// 待つ。
#[derive(Debug)]
pub struct TestOutputControl {
    enabled: AtomicBool,
    armed_run_id: AtomicU64,
}

impl Default for TestOutputControl {
    fn default() -> Self {
        Self::new()
    }
}

impl TestOutputControl {
    /// 常に無効・`armed_run_id`未設定で構築する。
    pub fn new() -> Self {
        Self {
            enabled: AtomicBool::new(false),
            armed_run_id: AtomicU64::new(NO_RUN),
        }
    }

    /// `run_id`に対してテスト出力を有効化する。既に別の run_id で有効化
    /// 済みだった場合も、この呼び出しの`run_id`だけが以後
    /// [`Self::is_active_for`]にマッチする（呼び出し順に置き換わる - 排他
    /// ロックは要らない、こういう置き換え自体が「今の run にだけ」という
    /// 要件を満たす）。
    pub fn enable(&self, run_id: RunId) {
        self.armed_run_id.store(run_id, Ordering::SeqCst);
        self.enabled.store(true, Ordering::SeqCst);
    }

    /// 無効化する。`armed_run_id`も`NO_RUN`へ戻す -
    /// `WriteControl::was_enabled_before_restart`のような表示専用の履歴を
    /// 残す意味がここには無い（このモジュールの doc comment「一切永続化
    /// しない」参照）。`CollectionController`の`start_locked`/
    /// `stop_locked`/モード切替の各遷移点から呼ばれ、収集の停止・新規
    /// 開始・モード切替・（非永続なのでプロセス終了・サービス再起動も
    /// 自動的に含む）のすべてで無効へ戻ることを保証する。
    pub fn disable(&self) {
        self.enabled.store(false, Ordering::SeqCst);
        self.armed_run_id.store(NO_RUN, Ordering::SeqCst);
    }

    /// いま有効か(run_id を問わない生のフラグ)。
    pub fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::SeqCst)
    }

    /// `run_id`(`None`は収集停止中/run コンテキストが無い状態)に対して
    /// テスト出力が有効か。有効化時と異なる run_id(モード切替・再開始で
    /// 新しい run_id が割り当てられた後)には決してマッチしない - 「現在の
    /// run コンテキストのみ」を実装する核心の判定。
    pub fn is_active_for(&self, run_id: Option<RunId>) -> bool {
        let Some(run_id) = run_id else {
            return false;
        };
        self.is_enabled() && self.armed_run_id.load(Ordering::SeqCst) == run_id
    }

    pub fn status(&self) -> TestOutputStatus {
        let armed = self.armed_run_id.load(Ordering::SeqCst);
        TestOutputStatus {
            enabled: self.is_enabled(),
            run_id: (armed != NO_RUN).then_some(armed),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starts_disabled_with_no_armed_run() {
        let control = TestOutputControl::new();
        assert!(!control.is_enabled());
        assert_eq!(
            control.status(),
            TestOutputStatus {
                enabled: false,
                run_id: None
            }
        );
        assert!(!control.is_active_for(Some(1)));
        assert!(!control.is_active_for(None));
    }

    #[test]
    fn enable_arms_for_the_given_run_id_only() {
        let control = TestOutputControl::new();
        control.enable(7);
        assert!(control.is_enabled());
        assert!(control.is_active_for(Some(7)));
        assert!(
            !control.is_active_for(Some(8)),
            "a different run_id must not match"
        );
        assert!(
            !control.is_active_for(None),
            "no run context must not match"
        );
        assert_eq!(
            control.status(),
            TestOutputStatus {
                enabled: true,
                run_id: Some(7)
            }
        );
    }

    #[test]
    fn disable_clears_both_the_flag_and_the_armed_run_id() {
        let control = TestOutputControl::new();
        control.enable(3);
        control.disable();
        assert!(!control.is_enabled());
        assert!(!control.is_active_for(Some(3)));
        assert_eq!(
            control.status(),
            TestOutputStatus {
                enabled: false,
                run_id: None
            }
        );
    }

    #[test]
    fn re_enabling_for_a_new_run_id_replaces_the_old_one() {
        let control = TestOutputControl::new();
        control.enable(1);
        control.enable(2);
        assert!(
            !control.is_active_for(Some(1)),
            "the previous run_id must stop matching once a new one is armed"
        );
        assert!(control.is_active_for(Some(2)));
    }
}
