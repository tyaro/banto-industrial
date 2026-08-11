//! Desktop↔Service 切替ウィザード用の最小 Tauri invoke 面
//! （docs/banto-hub-desktop-plan.md §9.7、docs/banto-hub-t16-design.md）。
//!
//! ## このモジュールがやること・やらないこと
//!
//! - **やる**: SCM 状態照会、Desktop↔Service 切替の開始、自動起動の UAC
//!   昇格起動、進捗イベント配信。いずれもシェル composition
//!   （どのホストで走るか）に属する。
//! - **やらない**: タグ CRUD・収集開始/停止などの運転 API（Hub REST のまま）。
//!
//! `banto-hub-elev.exe` の探索順（`set_service_autostart`）:
//! 1. このシェル実行ファイルと同じディレクトリ
//! 2. 親ディレクトリ直下 / `_verify_t17` 配下（staging 配置向け）
//! 3. カレントディレクトリ / `_verify_t17` 配下（開発時）

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use serde::Serialize;
use tauri::{AppHandle, Emitter, State};

#[cfg(windows)]
use crate::{build_service_manager, run_host_switch};
use crate::{AppState, ShellView};
#[cfg(windows)]
use banto_hub_core::host_switch::SwitchCommand;
#[cfg(windows)]
use banto_hub_core::service_elevated::ElevatedAction;
#[cfg(windows)]
use banto_hub_core::service_manager::ServiceManager;

/// 切替が進行中かどうかのプロセス全体フラグ（トレイと invoke で共有）。
pub(crate) static SWITCHING: AtomicBool = AtomicBool::new(false);

/// [`host_switch_status`]の戻り値。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HostSwitchStatusDto {
    /// SCM 状態の表示用文字列（未インストール等含む）。照会失敗時は`null`。
    pub scm_state: Option<String>,
    /// 次回 Windows 起動で自動起動するか（Demand のときは`false`）。
    pub auto_start: bool,
    /// Operators または Administrators（起動時確定値）。
    pub can_operate: bool,
    /// `"desktop"` / `"service"` / `"fallback"`。
    pub view: String,
    /// 切替トランザクション進行中か。
    pub switching: bool,
}

/// [`host_switch_progress`]イベントのペイロード。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HostSwitchProgressEvent {
    /// 短いフェーズ名（`starting` / `running` / `completed` / `faulted` 等）。
    pub phase: String,
    /// 利用者向け文言。
    pub message: String,
    /// 終端に達したか。
    pub done: bool,
    /// 失敗時の理由（成功時は`null`）。
    pub error: Option<String>,
}

/// 切替開始を試みる。既に進行中なら`false`。
pub(crate) fn try_begin_switch() -> bool {
    SWITCHING
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_ok()
}

/// 切替終了時に呼ぶ。
pub(crate) fn end_switch() {
    SWITCHING.store(false, Ordering::SeqCst);
}

/// 進捗イベントを全ウィンドウへ配信する。
pub(crate) fn emit_progress(app: &AppHandle, event: HostSwitchProgressEvent) {
    let _ = app.emit("host_switch_progress", event);
}

/// トレイ経由の開始も同じ進行中フラグを使う。既に進行中なら進捗イベントで通知し`false`。
pub(crate) fn begin_switch_or_warn(app: &AppHandle) -> bool {
    if try_begin_switch() {
        true
    } else {
        emit_progress(
            app,
            HostSwitchProgressEvent {
                phase: "busy".into(),
                message: "既に切替処理が進行中です".into(),
                done: true,
                error: Some("既に切替処理が進行中です".into()),
            },
        );
        false
    }
}

/// SCM / ビュー / 権限のスナップショットを返す。
#[tauri::command]
pub fn host_switch_status(state: State<'_, AppState>) -> HostSwitchStatusDto {
    let view = match &*state.view.lock().expect("view mutex poisoned") {
        ShellView::Desktop => "desktop".to_string(),
        ShellView::Service { .. } => "service".to_string(),
        ShellView::Fallback(_) => "fallback".to_string(),
    };
    let (scm_state, auto_start) = query_scm_summary();
    HostSwitchStatusDto {
        scm_state,
        auto_start,
        can_operate: state.can_operate_service,
        view,
        switching: SWITCHING.load(Ordering::SeqCst),
    }
}

fn query_scm_summary() -> (Option<String>, bool) {
    #[cfg(windows)]
    {
        let manager = build_service_manager();
        match manager.query_status() {
            Ok(status) => (Some(status.state.to_string()), status.auto_start),
            Err(_) => (None, false),
        }
    }
    #[cfg(not(windows))]
    {
        (None, false)
    }
}

/// Desktop/Offline → Service。権限なし・進行中は即エラー文字列。
#[tauri::command]
pub fn switch_to_service(app: AppHandle, state: State<'_, AppState>) -> Result<(), String> {
    if !state.can_operate_service {
        return Err("サービス操作の権限がありません（BantoHub Operators または管理者）".into());
    }
    if !begin_switch_or_warn(&app) {
        return Err("既に切替処理が進行中です".into());
    }
    emit_progress(
        &app,
        HostSwitchProgressEvent {
            phase: "starting".into(),
            message: "サービスへの切替を開始しています…".into(),
            done: false,
            error: None,
        },
    );
    #[cfg(windows)]
    {
        run_host_switch(&app, SwitchCommand::SwitchToService);
        Ok(())
    }
    #[cfg(not(windows))]
    {
        end_switch();
        Err("Windows 専用です".into())
    }
}

/// Service → Desktop。
#[tauri::command]
pub fn switch_to_desktop(app: AppHandle, state: State<'_, AppState>) -> Result<(), String> {
    if !state.can_operate_service {
        return Err("サービス操作の権限がありません（BantoHub Operators または管理者）".into());
    }
    if !begin_switch_or_warn(&app) {
        return Err("既に切替処理が進行中です".into());
    }
    emit_progress(
        &app,
        HostSwitchProgressEvent {
            phase: "starting".into(),
            message: "アプリへの切替を開始しています…".into(),
            done: false,
            error: None,
        },
    );
    #[cfg(windows)]
    {
        run_host_switch(&app, SwitchCommand::SwitchToDesktop);
        Ok(())
    }
    #[cfg(not(windows))]
    {
        end_switch();
        Err("Windows 専用です".into())
    }
}

/// 自動起動の ON/OFF。UAC 経由で `banto-hub-elev.exe` を起動する。
#[tauri::command]
pub fn set_service_autostart(state: State<'_, AppState>, enabled: bool) -> Result<(), String> {
    if !state.can_operate_service {
        return Err("サービス操作の権限がありません（BantoHub Operators または管理者）".into());
    }
    #[cfg(windows)]
    {
        let action = if enabled {
            ElevatedAction::AUTOSTART_ENABLE
        } else {
            ElevatedAction::AUTOSTART_DISABLE
        };
        let elev = resolve_elev_exe().ok_or_else(|| {
            "banto-hub-elev.exe が見つかりません（シェルと同じディレクトリに配置してください）"
                .to_string()
        })?;
        run_elev_uac(&elev, action)
    }
    #[cfg(not(windows))]
    {
        let _ = enabled;
        Err("Windows 専用です".into())
    }
}

/// `banto-hub-elev.exe` の探索（モジュール doc の探索順）。
pub(crate) fn resolve_elev_exe() -> Option<PathBuf> {
    let current = std::env::current_exe().ok()?;
    let dir = current.parent()?;
    let candidate = dir.join("banto-hub-elev.exe");
    if candidate.is_file() {
        return Some(candidate);
    }
    if let Some(parent) = dir.parent() {
        let staged = parent.join("_verify_t17").join("banto-hub-elev.exe");
        if staged.is_file() {
            return Some(staged);
        }
        let sibling = parent.join("banto-hub-elev.exe");
        if sibling.is_file() {
            return Some(sibling);
        }
    }
    let cwd = std::env::current_dir().ok()?;
    let from_cwd = cwd.join("banto-hub-elev.exe");
    if from_cwd.is_file() {
        return Some(from_cwd);
    }
    let verify = cwd.join("_verify_t17").join("banto-hub-elev.exe");
    if verify.is_file() {
        return Some(verify);
    }
    None
}

#[cfg(windows)]
fn run_elev_uac(elev: &Path, action: &str) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::{CloseHandle, WAIT_OBJECT_0};
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, WaitForSingleObject, INFINITE,
    };
    use windows_sys::Win32::UI::Shell::{
        ShellExecuteExW, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    let file: Vec<u16> = elev
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let params: Vec<u16> = action.encode_utf16().chain(std::iter::once(0)).collect();
    let verb: Vec<u16> = "runas".encode_utf16().chain(std::iter::once(0)).collect();

    let mut info = unsafe { std::mem::zeroed::<SHELLEXECUTEINFOW>() };
    info.cbSize = std::mem::size_of::<SHELLEXECUTEINFOW>() as u32;
    info.fMask = SEE_MASK_NOCLOSEPROCESS;
    info.lpVerb = verb.as_ptr();
    info.lpFile = file.as_ptr();
    info.lpParameters = params.as_ptr();
    info.nShow = SW_SHOWNORMAL;

    // SAFETY: 文字列は NUL 終端でこのスコープで生存する。
    let ok = unsafe { ShellExecuteExW(&mut info) };
    if ok == 0 {
        return Err("UAC の起動に失敗しました（拒否されたか、elev を起動できません）".into());
    }
    if info.hProcess.is_null() {
        return Err("昇格プロセスのハンドルを取得できませんでした".into());
    }
    // SAFETY: ShellExecuteExW が返した有効なプロセスハンドル。
    let wait = unsafe { WaitForSingleObject(info.hProcess, INFINITE) };
    let mut exit_code: u32 = 1;
    if wait == WAIT_OBJECT_0 {
        unsafe {
            GetExitCodeProcess(info.hProcess, &mut exit_code);
        }
    }
    unsafe {
        CloseHandle(info.hProcess);
    }
    if exit_code == 0 {
        Ok(())
    } else {
        Err(format!(
            "banto-hub-elev が失敗しました (exit code {exit_code})"
        ))
    }
}
