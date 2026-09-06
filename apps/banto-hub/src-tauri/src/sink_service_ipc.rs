//! S6（docs/banto-hub-external-db-design.md §5.5・§7 row S6、2026-09-07）:
//! デスクトップシェルの「サービス」一覧が使う `BantoHubSink` 専用の
//! Tauri invoke 面。
//!
//! `BantoHub` 自身の起動・停止は [`crate::host_switch_ipc`]
//! （[`banto_hub_core::host_switch::HostSwitchEngine`] 経由 - シェル自身が
//! Desktop/Service のどちらのホストとして走るかを決める複雑な状態機械）が
//! 担うが、`BantoHubSink` はこのシェルのホスト選択と無関係な独立サービス
//! （サイドカー、外部 DB 連携 S5）なので、
//! [`banto_hub_core::service_manager::WindowsServiceManager::for_service`]
//! （S6 で追加したサービス名パラメータ化 - `service_manager.rs` のモジュール
//! doc「サービス名の汎化」節参照）を直接 query/start/stop するだけの薄い
//! ラッパーにしてある。`HostSwitchEngine`・進捗イベント・`SWITCHING`
//! フラグはいずれも使わない - Sink には desktop/service の二重ホストという
//! 概念が無く、SCM 上の単純な1サービスとして扱えば足りるため
//! （設計 §5.5「SCM の状態照会・起動停止は T17 の実装を流用する」を、
//! 新しい特権コマンドを増やさず既存の `ServiceManager` trait のパラメータ化
//! だけで満たす）。
//!
//! ## 既知の制限（PR 本文にも記載、実機 Windows 未検証）
//!
//! `service_manager.rs` のモジュール doc「サービス名の汎化」節の既知の制限
//! を参照。`BantoHub Operators` グループには `BantoHubSink` サービス
//! オブジェクトへの ACE が付与されていない（`service_elevated.rs`
//! の `grant-service-acl` は `BantoHub` 固定）ため、Operators メンバー
//! （フルの Administrator ではない一般オペレータ）による start/stop は
//! 現状 [`banto_hub_core::service_manager::ServiceManagerError::AccessDenied`]
//! になりうる。フル Administrator であれば動作するはずだが、この worktree
//! では `BantoHubSink` サービス自体をインストールしない制約があるため未検証。
//! 状態照会（`query_status`、読み取り専用）は `service_manager.rs` の
//! `windows_for_service_tests` で実機確認済み。

use serde::Serialize;
use tauri::State;

use crate::AppState;

#[cfg(windows)]
use banto_hub_core::service_manager::{
    ServiceManager, ServiceManagerError, WindowsServiceManager, SINK_SERVICE_DISPLAY_NAME,
    SINK_SERVICE_NAME,
};

/// [`sink_service_status`]の戻り値。[`crate::host_switch_ipc::HostSwitchStatusDto`]
/// と似た形だが、Sink には `view`（desktop/service/fallback）の概念が無い
/// ため持たない - また `autoStart` も現時点では扱わない（このスライスは
/// query/start/stop のみが対象、モジュール doc参照）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SinkServiceStatusDto {
    /// SCM 状態の表示用文字列（[`banto_hub_core::service_manager::ScmState`]
    /// の`Display`）。照会失敗時は`null`。
    pub scm_state: Option<String>,
    /// Operators または Administrators（起動時確定値、`BantoHub`と共通の
    /// 判定 - モジュール doc「既知の制限」節参照。この値が`true`でも
    /// `BantoHubSink`への ACE が無い環境では実際の start/stop が
    /// `AccessDenied` になりうる）。
    pub can_operate: bool,
}

#[cfg(windows)]
fn build_sink_service_manager() -> WindowsServiceManager {
    WindowsServiceManager::for_service(
        SINK_SERVICE_NAME,
        SINK_SERVICE_DISPLAY_NAME,
        // `set_auto_start`はこのスライスでは呼ばない（モジュール doc参照）ので
        // 実際に登録されている実行ファイルパスと一致している必要は無い。
        std::env::current_exe().unwrap_or_default(),
        Vec::new(),
    )
}

#[cfg(windows)]
fn describe_error(err: ServiceManagerError) -> String {
    err.to_string()
}

/// `BantoHubSink`の SCM 状態を照会する（読み取り専用、権限不問）。
#[tauri::command]
pub fn sink_service_status(state: State<'_, AppState>) -> SinkServiceStatusDto {
    SinkServiceStatusDto {
        scm_state: query_sink_scm_state(),
        can_operate: state.can_operate_service,
    }
}

fn query_sink_scm_state() -> Option<String> {
    #[cfg(windows)]
    {
        let manager = build_sink_service_manager();
        manager.query_status().ok().map(|s| s.state.to_string())
    }
    #[cfg(not(windows))]
    {
        None
    }
}

/// `BantoHubSink`を開始する（[`ServiceManager::start`]と同様に冪等）。
#[tauri::command]
pub fn sink_service_start(state: State<'_, AppState>) -> Result<(), String> {
    if !state.can_operate_service {
        return Err("サービス操作の権限がありません（BantoHub Operators または管理者）".into());
    }
    #[cfg(windows)]
    {
        let manager = build_sink_service_manager();
        manager.start().map(|_| ()).map_err(describe_error)
    }
    #[cfg(not(windows))]
    {
        Err("Windows 専用です".into())
    }
}

/// `BantoHubSink`を停止する（冪等）。
#[tauri::command]
pub fn sink_service_stop(state: State<'_, AppState>) -> Result<(), String> {
    if !state.can_operate_service {
        return Err("サービス操作の権限がありません（BantoHub Operators または管理者）".into());
    }
    #[cfg(windows)]
    {
        let manager = build_sink_service_manager();
        manager.stop().map(|_| ()).map_err(describe_error)
    }
    #[cfg(not(windows))]
    {
        Err("Windows 専用です".into())
    }
}
