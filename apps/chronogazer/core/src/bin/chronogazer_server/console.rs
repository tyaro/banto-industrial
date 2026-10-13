//! `chronogazer-server` の Windows のコンソールの扱い（#392 B1-5）: 標準出力・
//! 標準エラー出力のログファイルへの向け直し（[`redirect_stdio`]）と、コンソールの
//! Ctrl の受け取り（[`install_ctrl_handler`]）。
//!
//! # 向け直し（`SetStdHandle` + 匿名パイプ）
//!
//! core と依存 crate は `eprintln!` を直接使う（30 か所以上）ので、出力の口
//! そのもの（プロセスの標準ハンドル）をパイプの書き込み側に差し替える。Rust の
//! `std::io::{stdout, stderr}` は書くたびに `GetStdHandle` を呼ぶので、差し替えた
//! 後の `println!`/`eprintln!` はすべてパイプへ行く。読み取り側のスレッドが行ごとに
//!
//! 1. ログファイル（`server_host::ServerLog`。時刻を付け、日付で切り替える）へ書き、
//! 2. 元の標準エラー出力（コンソール）へも写す（tee）。
//!
//! 写すのは別のスレッドで、間に上限つきの列を挟み、溢れたら捨てる: コンソールへの
//! 書き込みは止まりうる（QuickEdit で文字を選択している間はコンソールの出力が
//! 止まる）ので、それでログファイルへの書き込みや、`eprintln!` を呼んだ収集の
//! スレッドが止まらないようにする。パイプの書き込み側のバッファは 64 KiB。
//!
//! 終了のとき（[`StdioRedirect::finish`]）は、標準ハンドルを元に戻してから
//! パイプの書き込み側を閉じ、読み取り側が残りを書き終えてファイルを
//! `sync_all` するのを上限つきで待つ。**何度呼んでも、どのスレッドから呼んでも、
//! 最初の呼び出しが終わるまで後の呼び出しも戻らない**（Ctrl のハンドラーのスレッドと
//! main のスレッドの両方が呼ぶ。片方がプロセスを終わらせる前に、もう片方の
//! 書き出しを切らない）。
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

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use chronogazer_core::server_host::ServerLog;
use windows_sys::core::BOOL;
use windows_sys::Win32::Foundation::{CloseHandle, FALSE, HANDLE, INVALID_HANDLE_VALUE, TRUE};
use windows_sys::Win32::Storage::FileSystem::{ReadFile, WriteFile};
use windows_sys::Win32::System::Console::{
    GetConsoleMode, GetStdHandle, SetConsoleCtrlHandler, SetStdHandle, WriteConsoleW,
    CTRL_BREAK_EVENT, CTRL_CLOSE_EVENT, CTRL_C_EVENT, CTRL_LOGOFF_EVENT, CTRL_SHUTDOWN_EVENT,
    STD_ERROR_HANDLE, STD_OUTPUT_HANDLE,
};
use windows_sys::Win32::System::Pipes::CreatePipe;

/// パイプの書き込み側のバッファ。
const PIPE_BUFFER: u32 = 64 * 1024;
/// コンソールへ写す行の列の上限（溢れたら捨てる）。
const TEE_QUEUE: usize = 4096;

/// `HANDLE`（生ポインタ）をスレッドの間で渡すための入れ物。
#[derive(Clone, Copy)]
struct RawHandle(usize);

impl RawHandle {
    fn new(handle: HANDLE) -> Self {
        Self(handle as usize)
    }

    fn get(self) -> HANDLE {
        self.0 as HANDLE
    }

    fn is_usable(self) -> bool {
        let handle = self.get();
        !handle.is_null() && handle != INVALID_HANDLE_VALUE
    }
}

/// 向け直しの後始末の口（[`StdioRedirect::finish`]）。
pub struct StdioRedirect {
    write: RawHandle,
    original_out: RawHandle,
    original_err: RawHandle,
    done: Mutex<Option<Receiver<()>>>,
    finished: AtomicBool,
}

/// 標準出力・標準エラー出力を `log` へ向け直し、元の標準エラー出力にも写す。
pub fn redirect_stdio(log: Arc<Mutex<ServerLog>>) -> std::io::Result<StdioRedirect> {
    let mut read: HANDLE = std::ptr::null_mut();
    let mut write: HANDLE = std::ptr::null_mut();
    // SAFETY: 出力の 2 つのポインタは有効なローカル変数。属性は既定（継承しない）。
    if unsafe { CreatePipe(&mut read, &mut write, std::ptr::null(), PIPE_BUFFER) } == FALSE {
        return Err(std::io::Error::last_os_error());
    }
    let read = RawHandle::new(read);
    let write = RawHandle::new(write);
    // SAFETY: 標準ハンドルの取得は副作用が無い。
    let original_out = RawHandle::new(unsafe { GetStdHandle(STD_OUTPUT_HANDLE) });
    let original_err = RawHandle::new(unsafe { GetStdHandle(STD_ERROR_HANDLE) });

    let (tee_tx, tee_rx) = mpsc::sync_channel::<String>(TEE_QUEUE);
    if original_err.is_usable() {
        std::thread::Builder::new()
            .name("server-log-tee".into())
            .spawn(move || tee_to_console(original_err, tee_rx))?;
    }
    let (done_tx, done_rx) = mpsc::channel();
    std::thread::Builder::new()
        .name("server-log".into())
        .spawn(move || {
            pump(read, &log, original_err.is_usable().then_some(tee_tx));
            if let Ok(mut log) = log.lock() {
                log.flush();
            }
            // SAFETY: 読み取り側はこのスレッドだけが持つ。
            unsafe { CloseHandle(read.get()) };
            let _ = done_tx.send(());
        })?;

    // ここから先の `println!`/`eprintln!` はパイプへ行く。
    // SAFETY: 書き込み側のハンドルは `finish` で閉じるまで有効。
    unsafe {
        SetStdHandle(STD_OUTPUT_HANDLE, write.get());
        SetStdHandle(STD_ERROR_HANDLE, write.get());
    }
    Ok(StdioRedirect {
        write,
        original_out,
        original_err,
        done: Mutex::new(Some(done_rx)),
        finished: AtomicBool::new(false),
    })
}

/// パイプを読み、行ごとにログと（あれば）コンソールの列へ渡す。書き込み側が
/// すべて閉じられる（[`StdioRedirect::finish`]）と終わる。
fn pump(read: RawHandle, log: &Mutex<ServerLog>, tee: Option<SyncSender<String>>) {
    let mut buffer = vec![0_u8; 8192];
    let mut pending: Vec<u8> = Vec::new();
    let emit = |bytes: &[u8]| {
        let mut text = String::from_utf8_lossy(bytes).into_owned();
        while text.ends_with('\n') || text.ends_with('\r') {
            text.pop();
        }
        if let Ok(mut log) = log.lock() {
            log.write_line(&text);
        }
        if let Some(tee) = &tee {
            // 溢れた（コンソールが止まっている）・写すスレッドが無いなら捨てる。
            let _ = tee.try_send(text);
        }
    };
    loop {
        let mut count = 0_u32;
        // SAFETY: バッファは有効で、大きさを渡している。同期 I/O（overlapped なし）。
        let ok = unsafe {
            ReadFile(
                read.get(),
                buffer.as_mut_ptr(),
                buffer.len() as u32,
                &mut count,
                std::ptr::null_mut(),
            )
        };
        if ok == FALSE || count == 0 {
            break;
        }
        pending.extend_from_slice(&buffer[..count as usize]);
        while let Some(pos) = pending.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = pending.drain(..=pos).collect();
            emit(&line);
        }
    }
    if !pending.is_empty() {
        emit(&pending);
    }
}

/// 列の行を元の標準エラー出力へ書く（コンソールなら UTF-16 で `WriteConsoleW`、
/// それ以外（パイプ・ファイル）なら UTF-8 で `WriteFile`）。失敗は捨てる。
fn tee_to_console(target: RawHandle, rx: Receiver<String>) {
    let mut mode = 0;
    // SAFETY: 出力のポインタは有効なローカル変数。
    let is_console = unsafe { GetConsoleMode(target.get(), &mut mode) } != FALSE;
    for line in rx {
        if is_console {
            let wide: Vec<u16> = format!("{line}\r\n").encode_utf16().collect();
            let mut written = 0;
            // SAFETY: バッファと長さは `wide` のもの。
            unsafe {
                WriteConsoleW(
                    target.get(),
                    wide.as_ptr(),
                    wide.len() as u32,
                    &mut written,
                    std::ptr::null(),
                )
            };
        } else {
            let bytes = format!("{line}\n").into_bytes();
            let mut written = 0;
            // SAFETY: バッファと長さは `bytes` のもの。
            unsafe {
                WriteFile(
                    target.get(),
                    bytes.as_ptr(),
                    bytes.len() as u32,
                    &mut written,
                    std::ptr::null_mut(),
                )
            };
        }
    }
}

impl StdioRedirect {
    /// 標準ハンドルを元に戻し、残りをログに書き終えるのを `wait` まで待つ
    /// （モジュール doc「向け直し」。2 回目以降の呼び出しも、最初の呼び出しが
    /// 終わるまで戻らない）。
    pub fn finish(&self, wait: Duration) {
        let mut done = self.done.lock().unwrap_or_else(|p| p.into_inner());
        if self.finished.swap(true, Ordering::SeqCst) {
            return;
        }
        // SAFETY: 元のハンドルに戻す。書き込み側は自分だけが閉じる（1 回だけ）。
        unsafe {
            SetStdHandle(STD_OUTPUT_HANDLE, self.original_out.get());
            SetStdHandle(STD_ERROR_HANDLE, self.original_err.get());
            CloseHandle(self.write.get());
        }
        if let Some(rx) = done.take() {
            let _ = rx.recv_timeout(wait);
        }
    }
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
