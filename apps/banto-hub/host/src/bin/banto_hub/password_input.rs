//! `banto-hub create-admin` のパスワードの入力（上流 banto v8.0.0 の
//! `apps/admin-template/core/src/bin/banto_serve_parts/password_input.rs`、Issue #866・ADR-0002 の写し。
//! 振る舞いを変えたら上流と見比べる）。
//!
//! 端末からは入力を表示せず 2 回尋ねる。パイプ・ファイルからは 1 行目をそのまま読む（スクリプト・
//! 試験はこちら）。行の読み方は [`read_password_line`]（どの `BufRead` でも試せる）、端末の表示の
//! 切り替えは [`EchoOff`] だけが触る。端末の制御はこのファイルだけの小さな `unsafe` の FFI で、
//! 既にビルドに入っている `windows-sys`・`libc` を使う（`rpassword` は足さない。上流と同じ判断）。

use std::io::{self, BufRead, IsTerminal};
use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};

/// Reads one line and strips the trailing `\r\n` / `\n` (only those; spaces
/// are part of the password). EOF yields an empty string.
pub fn read_password_line<R: BufRead>(input: &mut R) -> io::Result<String> {
    let mut line = String::new();
    input.read_line(&mut line)?;
    Ok(line.trim_end_matches(['\r', '\n']).to_string())
}

/// Judges the two entries of a terminal session.
pub fn confirmation_matches(first: &str, second: &str) -> bool {
    first == second
}

/// Whether stdin is an interactive terminal (echo can and should be turned off).
pub fn stdin_is_terminal() -> bool {
    io::stdin().is_terminal()
}

/// Asks for the password twice on the terminal. Echo is turned off BEFORE the
/// first prompt is shown and restored only after the second line is read, so
/// nothing typed in between can appear (a type-ahead right after the prompt
/// included). If echo cannot be turned off this fails without reading: a
/// plain-text fallback would break the promise the prompt makes.
///
/// There is deliberately no interrupt handler: after Ctrl+C the terminal may
/// stay in no-echo mode (docs/recipes/no-login-app.md says how to get it
/// back). A handler would need the mode switch and the interrupt ordered
/// against each other and, on Unix, the whole signal disposition saved and
/// restored; that is not worth it for this one-off command.
pub fn read_password_twice_hidden(
    first_prompt: &str,
    second_prompt: &str,
) -> io::Result<(String, String)> {
    let guard = EchoOff::new()?;
    let mut stdin = io::stdin().lock();
    eprint!("{first_prompt}");
    let first = read_password_line(&mut stdin);
    eprintln!();
    let first = first?;
    eprint!("{second_prompt}");
    let second = read_password_line(&mut stdin);
    eprintln!();
    drop(guard);
    Ok((first, second?))
}

/// Set while an [`EchoOff`] exists: one terminal state is switched at a time.
static ACTIVE: AtomicBool = AtomicBool::new(false);

fn claim() -> io::Result<()> {
    if ACTIVE.swap(true, AtomicOrdering::SeqCst) {
        return Err(io::Error::other(
            "端末の入力の非表示は同時に 2 つ使えません",
        ));
    }
    Ok(())
}

/// Restores the previous console / terminal mode on drop.
struct EchoOff {
    #[cfg(windows)]
    handle: windows_sys::Win32::Foundation::HANDLE,
    #[cfg(windows)]
    saved: u32,
    #[cfg(unix)]
    saved: libc::termios,
}

#[cfg(windows)]
impl EchoOff {
    fn new() -> io::Result<Self> {
        use windows_sys::Win32::System::Console::{
            GetConsoleMode, GetStdHandle, SetConsoleMode, ENABLE_ECHO_INPUT, STD_INPUT_HANDLE,
        };
        claim()?;
        // SAFETY: plain Win32 calls on the process's own stdin handle; `saved`
        // is a valid out pointer for the duration of the call.
        let result = unsafe {
            let handle = GetStdHandle(STD_INPUT_HANDLE);
            let mut saved = 0u32;
            if GetConsoleMode(handle, &mut saved) == 0
                || SetConsoleMode(handle, saved & !ENABLE_ECHO_INPUT) == 0
            {
                Err(io::Error::last_os_error())
            } else {
                Ok(Self { handle, saved })
            }
        };
        if result.is_err() {
            ACTIVE.store(false, AtomicOrdering::SeqCst);
        }
        result
    }
}

#[cfg(windows)]
impl Drop for EchoOff {
    fn drop(&mut self) {
        use windows_sys::Win32::System::Console::SetConsoleMode;
        // SAFETY: restores the mode read in `new` on the same handle.
        unsafe {
            SetConsoleMode(self.handle, self.saved);
        }
        ACTIVE.store(false, AtomicOrdering::SeqCst);
    }
}

#[cfg(unix)]
impl EchoOff {
    fn new() -> io::Result<Self> {
        claim()?;
        // SAFETY: `termios` is plain data; `tcgetattr`/`tcsetattr` get a valid
        // pointer for the duration of the call, on fd 0 (stdin).
        let result = unsafe {
            let mut saved: libc::termios = std::mem::zeroed();
            if libc::tcgetattr(libc::STDIN_FILENO, &mut saved) != 0 {
                Err(io::Error::last_os_error())
            } else {
                let mut quiet = saved;
                quiet.c_lflag &= !libc::ECHO;
                if libc::tcsetattr(libc::STDIN_FILENO, libc::TCSAFLUSH, &quiet) != 0 {
                    Err(io::Error::last_os_error())
                } else {
                    Ok(Self { saved })
                }
            }
        };
        if result.is_err() {
            ACTIVE.store(false, AtomicOrdering::SeqCst);
        }
        result
    }
}

#[cfg(unix)]
impl Drop for EchoOff {
    fn drop(&mut self) {
        // SAFETY: restores the attributes read in `new`.
        unsafe {
            libc::tcsetattr(libc::STDIN_FILENO, libc::TCSAFLUSH, &self.saved);
        }
        ACTIVE.store(false, AtomicOrdering::SeqCst);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn pipe_path_reads_the_first_line_only() {
        let mut input = Cursor::new("s3cret pass\nignored\n");
        assert_eq!(read_password_line(&mut input).unwrap(), "s3cret pass");
    }

    #[test]
    fn crlf_is_stripped_but_spaces_are_kept() {
        let mut input = Cursor::new(" pw with spaces \r\n");
        assert_eq!(read_password_line(&mut input).unwrap(), " pw with spaces ");
    }

    #[test]
    fn no_trailing_newline_and_eof_are_handled() {
        assert_eq!(read_password_line(&mut Cursor::new("abc")).unwrap(), "abc");
        assert_eq!(read_password_line(&mut Cursor::new("")).unwrap(), "");
    }

    #[test]
    fn a_second_concurrent_guard_is_refused() {
        claim().unwrap();
        assert!(claim().is_err());
        ACTIVE.store(false, AtomicOrdering::SeqCst);
        claim().unwrap();
        ACTIVE.store(false, AtomicOrdering::SeqCst);
    }

    #[test]
    fn confirmation_must_match_exactly() {
        assert!(confirmation_matches("a b", "a b"));
        assert!(!confirmation_matches("abc", "abd"));
        assert!(!confirmation_matches("abc", "abc "));
    }
}
