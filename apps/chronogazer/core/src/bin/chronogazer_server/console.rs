//! `chronogazer-server` の Windows のコンソールの扱い（#392 B1-5）: 標準出力・
//! 標準エラー出力のログファイルへの向け直し（[`redirect_stdio`]）と、コンソールの
//! Ctrl の受け取り（[`install_ctrl_handler`]）。
//!
//! # 向け直し
//!
//! 仕組み（匿名パイプ・読み取りのスレッド・tee・番兵・[`StdioRedirect::finish`]・
//! [`redirect_stdio_to_nul`]）と約束は、デスクトップと共有するため
//! [`chronogazer_core::stdio_log`] へ移した（2026-10-24 オーナー決定「案 A」。振る舞いは
//! 同じ）。ここは日付の切り替えで起きる古いログの削除を、サービスなら追跡する口
//! （[`LogPrunes`]。STOP が緩める前に終わりを待つ）、コンソールなら切り離して走らせる
//! （`crate::run_log_prune`）ようにつなぐだけ。
//!
//! # Ctrl の受け取り（`SetConsoleCtrlHandler`）
//!
//! - `CTRL_C_EVENT`・`CTRL_BREAK_EVENT`: 終了の要求を記録してすぐ戻る（後始末は
//!   main のスレッドが [`chronogazer_core::server_host::INTERRUPT_BUDGET`] で行う）。
//! - `CTRL_CLOSE_EVENT`（コンソールのウィンドウを閉じた）・`CTRL_LOGOFF_EVENT`・
//!   `CTRL_SHUTDOWN_EVENT`: **ハンドラーから戻るとすぐプロセスが終わる**（OS は
//!   戻るのを約 5 秒まで待つ）ので、ハンドラーのスレッドで後始末の完了とログの
//!   書き出しを [`chronogazer_core::server_host::CLOSE_WAIT`] まで待ってから戻る。
//!   コンソール版はサービスではないので、ログオフ・シャットダウンでも終わる
//!   （同じ扱い）。なお、プロセスが `user32.dll` を読み込んでいると、OS は
//!   ログオフ・シャットダウンの通知をコンソールのハンドラーに送らない（Microsoft の
//!   文書）- そのときは通知なしに終わる。サインアウトをまたいで動かし続けるのは
//!   B3 のサービスの役目。

use std::sync::{Arc, Mutex, OnceLock};

use chronogazer_core::server_host::ServerLog;
pub use chronogazer_core::stdio_log::{redirect_stdio_to_nul, StdioRedirect};

use crate::log_prune::LogPrunes;
use windows_sys::core::BOOL;
use windows_sys::Win32::Foundation::{FALSE, TRUE};
use windows_sys::Win32::System::Console::{
    SetConsoleCtrlHandler, CTRL_BREAK_EVENT, CTRL_CLOSE_EVENT, CTRL_C_EVENT, CTRL_LOGOFF_EVENT,
    CTRL_SHUTDOWN_EVENT,
};

/// 標準出力・標準エラー出力を `log` へ向け直し、元の標準エラー出力にも写す
/// （[`chronogazer_core::stdio_log::redirect_stdio`]）。日付の切り替えで起きる古いログの
/// 削除は、`prunes` があればそこを通す（サービス。STOP が緩める前に終わりを待つ）。
/// 無ければ（コンソール）切り離して走らせる。
pub fn redirect_stdio(
    log: Arc<Mutex<ServerLog>>,
    prunes: Option<Arc<LogPrunes>>,
) -> std::io::Result<StdioRedirect> {
    chronogazer_core::stdio_log::redirect_stdio(
        log,
        Arc::new(move |prune| crate::run_log_prune(prunes.as_ref(), prune)),
    )
}

/// Ctrl の種類（[`install_ctrl_handler`] のコールバックに渡す）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CtrlKind {
    /// Ctrl-C・Ctrl-Break。
    Interrupt,
    /// コンソールのウィンドウを閉じた。
    Close,
    /// ログオフ。
    Logoff,
    /// シャットダウン。
    Shutdown,
}

type CtrlCallback = Box<dyn Fn(CtrlKind) + Send + Sync>;

static CTRL_CALLBACK: OnceLock<CtrlCallback> = OnceLock::new();

unsafe extern "system" fn ctrl_handler(ctrl_type: u32) -> BOOL {
    let kind = match ctrl_type {
        CTRL_C_EVENT | CTRL_BREAK_EVENT => CtrlKind::Interrupt,
        CTRL_CLOSE_EVENT => CtrlKind::Close,
        CTRL_LOGOFF_EVENT => CtrlKind::Logoff,
        CTRL_SHUTDOWN_EVENT => CtrlKind::Shutdown,
        _ => return FALSE,
    };
    match CTRL_CALLBACK.get() {
        Some(callback) => {
            // ハンドラーのスレッドで呼ぶ。Close 系は、後始末が終わるまで戻らない
            // （戻るとプロセスが終わる）のはコールバックの責任。
            callback(kind);
            TRUE
        }
        None => FALSE,
    }
}

/// Ctrl のハンドラーを登録する（プロセスで 1 回）。`callback` は OS が作る
/// ハンドラーのスレッドで呼ばれる。
pub fn install_ctrl_handler(callback: CtrlCallback) -> std::io::Result<()> {
    if CTRL_CALLBACK.set(callback).is_err() {
        return Err(std::io::Error::other("Ctrl のハンドラーは登録済みです"));
    }
    // SAFETY: 関数ポインタは 'static。
    if unsafe { SetConsoleCtrlHandler(Some(ctrl_handler), TRUE) } == FALSE {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

/// Ctrl のハンドラーをこのプロセスで登録したか（テスト用。#392 B3-2b: サービスの
/// 本体はコンソールのハンドラーを登録しない、を確かめる）。
#[cfg(test)]
pub fn ctrl_handler_installed() -> bool {
    CTRL_CALLBACK.get().is_some()
}
