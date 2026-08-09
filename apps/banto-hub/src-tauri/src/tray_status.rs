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
//! だけなので、tooltip のホスト名部分は常に「アプリ」固定
//! （サービス接続時の識別は T16-2）。

use banto_hub_core::controller::{CollectionState, RunMode, RuntimeStatus};

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
}
