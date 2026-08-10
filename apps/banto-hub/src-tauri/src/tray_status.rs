//! T16-1（`docs/banto-hub-t16-design.md` §3「T16-1」・
//! `docs/banto-hub-desktop-plan.md` §9.7 状態表示表 / §9.9 トレイ表）:
//! 収集状態 → トレイ表示（ラベル・tooltip・メニュー構成）への変換だけを
//! 行う純粋関数の集まり。
//!
//! Tauri の型（`AppHandle`/`TrayIcon`/`Menu` 等）に一切依存しないことで、
//! Tauri アプリの外形を起動せずに単体テストできるようにしている
//! （実装指示「ユニットテスト: 状態→表示ラベル変換の pure 関数を分離して
//! テストすること」）。呼び出し側（`lib.rs`）はこの結果を使って実際の
//! tooltip・メニューを組み立てる。
//!
//! T16-1 の範囲はデスクトップホスト＝アプリがランタイムを所有している場合
//! だけだったので、tooltip のホスト名部分は常に「アプリ」固定だった
//! （サービス接続時の識別は T16-2 で追加）。
//!
//! ## T16-2 で追加したもの（docs/banto-hub-t16-design.md §3「T16-2」）
//!
//! - [`service_status_label`]/[`service_tooltip_text`]: `HostKind::Service`
//!   （サービスへ接続中）のトレイ表示。
//! - [`fallback_status_label`]/[`fallback_tooltip_text`]: `HostKind::Offline`
//!   （デスクトップ起動失敗・サービス health 不良等の fallback）のトレイ表示。
//! - [`describe_health_outcome`]: fallback 画面本文・トレイ状態行に出す
//!   [`HealthOutcome`]の日本語診断文。
//! - [`show_start_service_action`]/[`show_stop_service_action`]: fallback/
//!   service トレイに「サービスを開始」「サービスを停止」を出すかどうかの
//!   判定（実装指示 3.・4.「Operators-gated」「Hide start when health
//!   outcomes are WrongProfileOrVersion / MutexOwnerUnknown / PortConflict /
//!   StartPending」）。純粋関数なので Tauri を起動せずテストできる
//!   （このモジュール doc 冒頭の方針と同じ）。

use banto_hub_core::controller::{CollectionState, RunMode, RuntimeStatus};
use banto_hub_core::hub_health::HealthOutcome;
use banto_hub_core::service_manager::ScmState;

/// 色だけに頼らない状態ラベル（desktop-plan §9.7 の状態表示表と同じ文言）。
pub fn status_label(status: &RuntimeStatus) -> &'static str {
    match status.state {
        CollectionState::Stopped => "収集停止",
        CollectionState::Starting => "開始中",
        CollectionState::Running => match status.mode {
            RunMode::Configured => "設定どおり運転",
            RunMode::AllSimulation => "全 PLC シミュレーション",
        },
        CollectionState::Stopping => "停止処理中",
        CollectionState::Faulted => "異常停止",
    }
}

/// トレイ tooltip 全文（実装指示「tooltip 例:
/// `banto-hub — アプリ — 収集停止`」）。
pub fn tooltip_text(status: &RuntimeStatus) -> String {
    format!("banto-hub — アプリ — {}", status_label(status))
}

/// トレイメニューへ「収集を停止」を出すかどうか
/// （desktop-plan §9.9「アプリ・収集中」= Starting/Running/Stopping のみ。
/// Faulted は実装指示どおり出さない - 「開始はトレイに置かない」のと同じ
/// 理由で、異常停止からの操作は本画面の診断へ誘導する）。
pub fn show_stop_item(status: &RuntimeStatus) -> bool {
    matches!(
        status.state,
        CollectionState::Starting | CollectionState::Running | CollectionState::Stopping
    )
}

/// `HostKind::Service`（サービスへ接続中）のトレイ状態行。SCM の
/// `ScmState::Display`をそのまま埋め込む - fallback とは違い、この状態に
/// 来る時点で probe は必ず[`HealthOutcome::Healthy`]（呼び出し元
/// `lib.rs::decide_startup`参照）なので、health 診断文は不要。
pub fn service_status_label(scm_state: &ScmState) -> String {
    format!("サービス: {scm_state}")
}

/// トレイ tooltip 全文（[`tooltip_text`]のサービス版 - ホスト名部分が
/// 「アプリ」ではなく「サービス」になる）。
pub fn service_tooltip_text(scm_state: &ScmState) -> String {
    format!("banto-hub — サービス — {scm_state}")
}

/// `HostKind::Offline`（fallback）のトレイ状態行 - 具体的な理由は本文
/// （`lib.rs::fallback_message`）側に出すので、ここは短い固定文言。
pub fn fallback_status_label() -> &'static str {
    "未接続"
}

/// トレイ tooltip 全文（fallback 版）。
pub fn fallback_tooltip_text() -> String {
    "banto-hub — 未接続".to_string()
}

/// [`HealthOutcome`]の日本語診断文（fallback 画面本文・状態行で共有する）。
pub fn describe_health_outcome(outcome: &HealthOutcome) -> String {
    match outcome {
        HealthOutcome::Healthy { version } => format!("応答あり（version {version}）"),
        HealthOutcome::Unreachable => {
            "応答がありません（サービスが応答していない可能性があります）".to_string()
        }
        HealthOutcome::PortConflict => {
            "ポートは使用中ですが、banto-hub とは異なる応答でした（別プロセスが使用している可能性があります）"
                .to_string()
        }
        HealthOutcome::WrongProfileOrVersion => {
            "別の profile または version の Hub が応答しています".to_string()
        }
        HealthOutcome::MutexOwnerUnknown => {
            "応答はありますが、profile の所有情報を確認できませんでした".to_string()
        }
    }
}

/// fallback/service トレイで「サービスを開始」を隠すべきかどうか
/// （実装指示 3.「Hide start when health outcomes are
/// WrongProfileOrVersion / MutexOwnerUnknown / PortConflict / StartPending」・
/// desktop-plan §9.9）。`StartPending`は`health`ではなく`scm_state`側の
/// 値だが、実装指示が同じ列挙に含めているとおりここでまとめて扱う。
fn hide_start_service_action(scm_state: Option<&ScmState>, health: Option<&HealthOutcome>) -> bool {
    if matches!(scm_state, Some(ScmState::StartPending)) {
        return true;
    }
    matches!(
        health,
        Some(HealthOutcome::WrongProfileOrVersion)
            | Some(HealthOutcome::MutexOwnerUnknown)
            | Some(HealthOutcome::PortConflict)
    )
}

/// 「サービスを開始」項目を出すかどうか（実装指示 4.「Operators-gated
/// 「サービスを開始」if Stopped and can_operate」）。`Stopped`以外
/// （`NotInstalled`は`ServiceManager::start`が`NotFound`で失敗する、
/// `StartPending`/`StopPending`/`Other`は遷移中で二重に叩くべきでない）
/// では常に隠す。
pub fn show_start_service_action(
    scm_state: Option<&ScmState>,
    health: Option<&HealthOutcome>,
    can_operate: bool,
) -> bool {
    can_operate
        && matches!(scm_state, Some(ScmState::Stopped))
        && !hide_start_service_action(scm_state, health)
}

/// 「サービスを停止」項目を出すかどうか（`ScmState::Running`かつ
/// Operators-gated のときだけ）。
pub fn show_stop_service_action(scm_state: &ScmState, can_operate: bool) -> bool {
    can_operate && *scm_state == ScmState::Running
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status_with(state: CollectionState, mode: RunMode) -> RuntimeStatus {
        RuntimeStatus {
            state,
            mode,
            run_id: None,
            last_error: None,
            configured_revision: 0,
            running_revision: 0,
        }
    }

    #[test]
    fn status_label_covers_every_state_and_mode() {
        assert_eq!(
            status_label(&status_with(CollectionState::Stopped, RunMode::Configured)),
            "収集停止"
        );
        assert_eq!(
            status_label(&status_with(CollectionState::Starting, RunMode::Configured)),
            "開始中"
        );
        assert_eq!(
            status_label(&status_with(CollectionState::Running, RunMode::Configured)),
            "設定どおり運転"
        );
        assert_eq!(
            status_label(&status_with(
                CollectionState::Running,
                RunMode::AllSimulation
            )),
            "全 PLC シミュレーション"
        );
        assert_eq!(
            status_label(&status_with(CollectionState::Stopping, RunMode::Configured)),
            "停止処理中"
        );
        assert_eq!(
            status_label(&status_with(CollectionState::Faulted, RunMode::Configured)),
            "異常停止"
        );
    }

    #[test]
    fn tooltip_text_prefixes_host_and_label() {
        let status = status_with(CollectionState::Running, RunMode::AllSimulation);
        assert_eq!(
            tooltip_text(&status),
            "banto-hub — アプリ — 全 PLC シミュレーション"
        );
    }

    #[test]
    fn show_stop_item_true_only_while_starting_running_or_stopping() {
        for mode in [RunMode::Configured, RunMode::AllSimulation] {
            assert!(!show_stop_item(&status_with(
                CollectionState::Stopped,
                mode
            )));
            assert!(show_stop_item(&status_with(
                CollectionState::Starting,
                mode
            )));
            assert!(show_stop_item(&status_with(CollectionState::Running, mode)));
            assert!(show_stop_item(&status_with(
                CollectionState::Stopping,
                mode
            )));
            assert!(!show_stop_item(&status_with(
                CollectionState::Faulted,
                mode
            )));
        }
    }

    #[test]
    fn service_labels_embed_scm_state() {
        assert_eq!(
            service_status_label(&ScmState::Running),
            "サービス: Running"
        );
        assert_eq!(
            service_tooltip_text(&ScmState::Running),
            "banto-hub — サービス — Running"
        );
    }

    #[test]
    fn fallback_labels_are_fixed_short_text() {
        assert_eq!(fallback_status_label(), "未接続");
        assert_eq!(fallback_tooltip_text(), "banto-hub — 未接続");
    }

    #[test]
    fn describe_health_outcome_covers_every_variant() {
        assert!(describe_health_outcome(&HealthOutcome::Healthy {
            version: "1.2.3".to_string()
        })
        .contains("1.2.3"));
        assert!(!describe_health_outcome(&HealthOutcome::Unreachable).is_empty());
        assert!(!describe_health_outcome(&HealthOutcome::PortConflict).is_empty());
        assert!(!describe_health_outcome(&HealthOutcome::WrongProfileOrVersion).is_empty());
        assert!(!describe_health_outcome(&HealthOutcome::MutexOwnerUnknown).is_empty());
    }

    #[test]
    fn show_start_service_action_requires_stopped_and_can_operate() {
        assert!(show_start_service_action(
            Some(&ScmState::Stopped),
            None,
            true
        ));
        // Operators でなければ隠す。
        assert!(!show_start_service_action(
            Some(&ScmState::Stopped),
            None,
            false
        ));
        // Stopped 以外（NotInstalled/Running/StartPending/StopPending/Other）は隠す。
        for state in [
            ScmState::NotInstalled,
            ScmState::Running,
            ScmState::StartPending,
            ScmState::StopPending,
            ScmState::Other("Paused".to_string()),
        ] {
            assert!(!show_start_service_action(Some(&state), None, true));
        }
        // scm_state が確認できていない（None）場合も隠す。
        assert!(!show_start_service_action(None, None, true));
    }

    #[test]
    fn show_start_service_action_hides_on_ambiguous_health_or_start_pending() {
        for health in [
            HealthOutcome::WrongProfileOrVersion,
            HealthOutcome::MutexOwnerUnknown,
            HealthOutcome::PortConflict,
        ] {
            assert!(!show_start_service_action(
                Some(&ScmState::Stopped),
                Some(&health),
                true
            ));
        }
        // Unreachable は「まだ health 未確認」なだけなので隠さない。
        assert!(show_start_service_action(
            Some(&ScmState::Stopped),
            Some(&HealthOutcome::Unreachable),
            true
        ));
        assert!(!show_start_service_action(
            Some(&ScmState::StartPending),
            None,
            true
        ));
    }

    #[test]
    fn show_stop_service_action_requires_running_and_can_operate() {
        assert!(show_stop_service_action(&ScmState::Running, true));
        assert!(!show_stop_service_action(&ScmState::Running, false));
        assert!(!show_stop_service_action(&ScmState::Stopped, true));
        assert!(!show_stop_service_action(&ScmState::StartPending, true));
    }
}
