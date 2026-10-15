//! 製品の elev を UAC で昇格して起動し、終わりを待って終了コードを返す
//! （`ShellExecuteExW` の verb `runas`）。
//!
//! 引数は**トークン**（英数字と `-` だけの空でない文字列）に限る:
//! `ShellExecuteExW` は引数を 1 本のコマンドラインとして渡し、受け取る側の
//! CRT がそれを分け直す。空白・引用符・`\`・メタ文字を通さなければ、分け方の
//! 規則の違いで別の引数に化けることが無い。アクション名（`autostart-enable`）・
//! オプション名（`--install-user-sid`）・サービス名（`BantoHubSink`）・SID
//! （`S-1-5-21-…`）はどれもこの形に収まる。
//!
//! 昇格を断られた・起動できない・ハンドルが無い・待てない・終了コードを
//! 読めない、を [`LaunchError`] で分ける（終了コードそのものの意味は製品の
//! elev が決める）。banto-hub のシェルは移す前の文言へ写す（banto-hub-shell の
//! `host_switch_ipc::elev_result_message`）。

use thiserror::Error;

/// [`run_elevated`] の失敗。
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum LaunchError {
    /// 引数にトークンの形でないものがある（[`is_valid_token`]）。何も起動していない。
    #[error("昇格して渡す引数 '{0}' に使えない文字があります（英数字と '-' だけ）")]
    InvalidToken(String),
    /// `ShellExecuteExW` が失敗した（UAC で「いいえ」= `ERROR_CANCELLED`(1223)、
    /// exe が無い等）。
    #[error("昇格して起動できませんでした (ShellExecuteExW, os error {0})")]
    ShellExecute(u32),
    /// 起動はしたがプロセスのハンドルが返らなかった。
    #[error("昇格したプロセスのハンドルを取得できませんでした")]
    NoProcessHandle,
    /// `WaitForSingleObject` が `WAIT_OBJECT_0` 以外を返した（値はその戻り値）。
    #[error("昇格したプロセスの終わりを待てませんでした (WaitForSingleObject, 戻り値 {0})")]
    Wait(u32),
    /// `GetExitCodeProcess` が失敗した。
    #[error("昇格したプロセスの終了コードを読めませんでした (GetExitCodeProcess, os error {0})")]
    ExitCode(u32),
}

/// 空でなく、英数字（ASCII）と `-` だけからなるか（純関数）。
pub fn is_valid_token(token: &str) -> bool {
    !token.is_empty()
        && token
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

/// トークンを確かめて 1 本の引数文字列（空白区切り）にする（純関数）。
/// 1 つでも形が違えば [`LaunchError::InvalidToken`]。空の列は空文字列。
pub fn parameters_from_tokens(tokens: &[&str]) -> Result<String, LaunchError> {
    if let Some(bad) = tokens.iter().find(|t| !is_valid_token(t)) {
        return Err(LaunchError::InvalidToken((*bad).to_string()));
    }
    Ok(tokens.join(" "))
}

/// `exe` を UAC で昇格して（verb `runas`、`SW_SHOWNORMAL`）`tokens` を引数に
/// 起動し、終わるまで待って終了コードを返す。
///
/// 呼ぶ側のスレッドは終わるまで止まる（上限なし - UAC の同意の窓を利用者が
/// 閉じるまで待つのと同じ。banto-hub のシェルは移す前からこの形）。
#[cfg(windows)]
pub fn run_elevated(exe: &std::path::Path, tokens: &[&str]) -> Result<u32, LaunchError> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, WAIT_OBJECT_0};
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, WaitForSingleObject, INFINITE,
    };
    use windows_sys::Win32::UI::Shell::{
        ShellExecuteExW, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    let parameters = parameters_from_tokens(tokens)?;

    let file: Vec<u16> = exe
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let params: Vec<u16> = parameters
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let verb: Vec<u16> = "runas".encode_utf16().chain(std::iter::once(0)).collect();

    // SAFETY: すべてのフィールドが 0／null で有効な C の構造体。
    let mut info = unsafe { std::mem::zeroed::<SHELLEXECUTEINFOW>() };
    info.cbSize = std::mem::size_of::<SHELLEXECUTEINFOW>() as u32;
    info.fMask = SEE_MASK_NOCLOSEPROCESS;
    info.lpVerb = verb.as_ptr();
    info.lpFile = file.as_ptr();
    info.lpParameters = params.as_ptr();
    info.nShow = SW_SHOWNORMAL;

    // SAFETY: 文字列は NUL 終端でこのスコープで生きている。
    if unsafe { ShellExecuteExW(&mut info) } == 0 {
        // SAFETY: 直前の呼び出しの失敗の理由を読むだけ。
        return Err(LaunchError::ShellExecute(unsafe { GetLastError() }));
    }
    if info.hProcess.is_null() {
        return Err(LaunchError::NoProcessHandle);
    }

    // SAFETY: `ShellExecuteExW` が返した有効なプロセスのハンドル（下で閉じる）。
    let wait = unsafe { WaitForSingleObject(info.hProcess, INFINITE) };
    let result = if wait != WAIT_OBJECT_0 {
        Err(LaunchError::Wait(wait))
    } else {
        let mut exit_code: u32 = 0;
        // SAFETY: 有効なハンドルと書き込み先。
        if unsafe { GetExitCodeProcess(info.hProcess, &mut exit_code) } == 0 {
            // SAFETY: 直前の呼び出しの失敗の理由を読むだけ。
            Err(LaunchError::ExitCode(unsafe { GetLastError() }))
        } else {
            Ok(exit_code)
        }
    };
    // SAFETY: 上のハンドルを 1 回だけ閉じる。
    unsafe { CloseHandle(info.hProcess) };
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_accept_actions_options_service_names_and_sids() {
        for good in [
            "autostart-enable",
            "service-install",
            "--install-user-sid",
            "BantoHubSink",
            "S-1-5-21-1111111111-2222222222-3333333333-1001",
            "S-1-12-1-1234567890-1234567890-1234567890-1234567890",
            "a",
            "0",
        ] {
            assert!(is_valid_token(good), "{good}");
        }
    }

    #[test]
    fn tokens_reject_anything_that_could_split_or_escape_the_command_line() {
        for bad in [
            "",
            " ",
            "a b",
            "a\tb",
            "\"",
            "a\"b",
            "'",
            "a\\b",
            "C:\\Program Files\\x.exe",
            "a;b",
            "a&b",
            "a|b",
            "a^b",
            "%PATH%",
            "a>b",
            "a<b",
            "a\nb",
            "a\0b",
            "a_b",
            "a.b",
            "a/b",
            "a=b",
            "ａ",
            "é",
        ] {
            assert!(!is_valid_token(bad), "{bad:?}");
        }
    }

    #[test]
    fn parameters_join_valid_tokens_with_single_spaces() {
        assert_eq!(parameters_from_tokens(&[]).unwrap(), "");
        assert_eq!(
            parameters_from_tokens(&["autostart-enable"]).unwrap(),
            "autostart-enable"
        );
        assert_eq!(
            parameters_from_tokens(&[
                "service-install",
                "--install-user-sid",
                "S-1-5-21-1-2-3-1001"
            ])
            .unwrap(),
            "service-install --install-user-sid S-1-5-21-1-2-3-1001"
        );
    }

    #[test]
    fn parameters_reject_the_first_bad_token() {
        assert_eq!(
            parameters_from_tokens(&["grant-service-acl", "Banto Hub", "x\"y"]),
            Err(LaunchError::InvalidToken("Banto Hub".to_string()))
        );
    }

    /// 形の違う引数では何も起動しない（`ShellExecuteExW` まで行かない）。
    #[cfg(windows)]
    #[test]
    fn run_elevated_rejects_bad_tokens_without_launching() {
        let exe = std::path::Path::new("Z:\\does-not-exist\\never-launched.exe");
        assert_eq!(
            run_elevated(exe, &["autostart-enable", "\" & calc"]),
            Err(LaunchError::InvalidToken("\" & calc".to_string()))
        );
    }
}
