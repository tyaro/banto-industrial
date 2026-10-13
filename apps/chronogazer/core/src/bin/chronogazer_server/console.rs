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
//! パイプへ**番兵の行**（[`PIPE_END`]）を書き、読み取り側が番兵より前の行を書き
//! 終えてファイルを `sync_all` するのを上限つきで待つ。**パイプの書き込み側は閉じない**
//! （#598 監査 F4）: `eprintln!` は書くたびに `GetStdHandle` → `WriteFile` を行うので、
//! 標準ハンドルを戻した直後に閉じると、その間にいた書き手が閉じた番号へ書きうる。
//! 番号はすぐ別のオブジェクト（DB の接続・データファイルなど）に再利用されうるので、
//! ログの行がそのファイルへ書かれかねない。書き込み側はプロセスの終わりまで開いた
//! まま（番号を再利用させない）で、読み取り側は番兵の後もパイプを読み続ける（遅れて
//! 来た行で書き手が詰まらないよう、ログへ書き続ける。書き出し・写しはしない）。**何度呼んでも、どのスレッドから呼んでも、
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
/// 向け直しの終わりの印（[`StdioRedirect::finish`] がパイプへ書く 1 行）。NUL を
/// 含むので、ふつうの出力の行とは重ならない。
const PIPE_END: &[u8] = b"\0chronogazer-server: end of the stdio redirect\0\n";

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
    /// [`StdioRedirect::write_through`] がパイプを通さずに書く先。
    log: Arc<Mutex<ServerLog>>,
    /// コンソールへ写す列（[`StdioRedirect::write_through`] も使う）。
    tee: Option<TeeQueue>,
}

/// コンソールへ写す列とそのワーカー（#598 の Copilot の指摘）。`finish` で
/// [`TeeQueue::close_and_wait`] を呼び、送り口を手放してワーカーが残りを書き終える
/// （列が空になって終わる）のを予算の範囲で待つ。送り口を持ち続けるとワーカーは
/// 終われず、`exit` の前のコンソールへの最後の写しが失われうる。
struct TeeQueue {
    tx: Mutex<Option<SyncSender<String>>>,
    done: Mutex<Option<Receiver<()>>>,
}

impl TeeQueue {
    /// ワーカーを起こす。`write` は 1 行をコンソール（など）へ書く。返す 2 つ目は
    /// パイプのポンプが使う送り口（ポンプが終わると手放す）。
    fn spawn(
        write: impl FnMut(&str) + Send + 'static,
    ) -> std::io::Result<(Self, SyncSender<String>)> {
        let (tx, rx) = mpsc::sync_channel::<String>(TEE_QUEUE);
        let (done_tx, done_rx) = mpsc::channel();
        let mut write = write;
        std::thread::Builder::new()
            .name("server-log-tee".into())
            .spawn(move || {
                for line in rx {
                    write(&line);
                }
                let _ = done_tx.send(());
            })?;
        Ok((
            Self {
                tx: Mutex::new(Some(tx.clone())),
                done: Mutex::new(Some(done_rx)),
            },
            tx,
        ))
    }

    /// 列に入れる（溢れた・もう閉じたなら捨てる）。
    fn send(&self, line: String) {
        if let Some(tx) = self.tx.lock().unwrap_or_else(|p| p.into_inner()).as_ref() {
            let _ = tx.try_send(line);
        }
    }

    /// 送り口を手放し、ワーカーが残りを書き終えて終わるのを `wait` まで待つ。
    /// 終わったら `true`。コンソールが止まっている（QuickEdit の選択中など）と
    /// 予算を超えては待たずに諦める（`false`。ログには既に書いてある）。他の送り口
    /// （パイプのポンプ）が残っていれば、それが手放すまでワーカーは終わらない。
    fn close_and_wait(&self, wait: Duration) -> bool {
        self.tx.lock().unwrap_or_else(|p| p.into_inner()).take();
        match self.done.lock().unwrap_or_else(|p| p.into_inner()).take() {
            Some(done) => done.recv_timeout(wait).is_ok(),
            None => true,
        }
    }
}

/// 標準出力・標準エラー出力を `log` へ向け直し、元の標準エラー出力にも写す。
pub fn redirect_stdio(log: Arc<Mutex<ServerLog>>) -> std::io::Result<StdioRedirect> {
    // SAFETY: 標準ハンドルの取得は副作用が無い。
    let original_out = RawHandle::new(unsafe { GetStdHandle(STD_OUTPUT_HANDLE) });
    let original_err = RawHandle::new(unsafe { GetStdHandle(STD_ERROR_HANDLE) });
    // 写しのワーカーはパイプより先に作る（#598 監査 F7: 作れなかったときにパイプの
    // ハンドルを漏らさない。ここで失敗すれば何も作っていない）。
    let (direct_tee, pump_tee) = if original_err.is_usable() {
        let (queue, pump_tx) = TeeQueue::spawn(console_writer(original_err))?;
        (Some(queue), Some(pump_tx))
    } else {
        (None, None)
    };

    let mut read: HANDLE = std::ptr::null_mut();
    let mut write: HANDLE = std::ptr::null_mut();
    // SAFETY: 出力の 2 つのポインタは有効なローカル変数。属性は既定（継承しない）。
    if unsafe { CreatePipe(&mut read, &mut write, std::ptr::null(), PIPE_BUFFER) } == FALSE {
        return Err(std::io::Error::last_os_error());
    }
    let read = RawHandle::new(read);
    let write = RawHandle::new(write);

    let direct_log = Arc::clone(&log);
    let (done_tx, done_rx) = mpsc::channel();
    let spawned = std::thread::Builder::new()
        .name("server-log".into())
        .spawn(move || {
            pump(read, &log, pump_tee, done_tx);
            // SAFETY: 読み取り側はこのスレッドだけが持つ（書き込み側は閉じないので、
            // ふつうはここに来ない）。
            unsafe { CloseHandle(read.get()) };
        });
    if let Err(err) = spawned {
        // まだ標準ハンドルを差し替えていない（誰も書き込み側を持っていない）ので、
        // 両方を閉じてよい（#598 監査 F7）。
        // SAFETY: どちらもこの関数だけが持つハンドル。
        unsafe {
            CloseHandle(read.get());
            CloseHandle(write.get());
        }
        return Err(err);
    }

    // ここから先の `println!`/`eprintln!` はパイプへ行く。両方の差し替えが成功した
    // ときだけ向け直したことにする（#598 の Copilot の指摘）。片方でも失敗したら
    // 元に戻し、パイプを閉じて `Err`（呼び出し側は起動を止める）。デバッグビルド
    // だけ、環境変数 `CHRONOGAZER_TEST_FAIL_STDIO_REDIRECT=1` で標準エラー出力の
    // 差し替えの失敗を注入できる（統合テスト用）。
    let inject_failure = cfg!(debug_assertions)
        && std::env::var("CHRONOGAZER_TEST_FAIL_STDIO_REDIRECT").as_deref() == Ok("1");
    // SAFETY: 書き込み側のハンドルはプロセスの終わりまで閉じない（有効なまま）。
    let out_ok = unsafe { SetStdHandle(STD_OUTPUT_HANDLE, write.get()) } != FALSE;
    let err_ok = out_ok
        && !inject_failure
        && unsafe { SetStdHandle(STD_ERROR_HANDLE, write.get()) } != FALSE;
    if !err_ok {
        let error = if inject_failure && out_ok {
            std::io::Error::other(
                "テスト用に標準エラー出力の差し替えの失敗を注入しました（CHRONOGAZER_TEST_FAIL_STDIO_REDIRECT）",
            )
        } else {
            std::io::Error::last_os_error()
        };
        // 元のハンドルに戻し、番兵を書く（書き込み側は閉じない。差し替えた標準出力を
        // 他のスレッドが掴んだかもしれないため。`finish` と同じ）。
        end_redirect(write, original_out, original_err);
        let _ = done_rx.recv_timeout(Duration::from_secs(1));
        return Err(error);
    }
    Ok(StdioRedirect {
        write,
        original_out,
        original_err,
        done: Mutex::new(Some(done_rx)),
        finished: AtomicBool::new(false),
        log: direct_log,
        tee: direct_tee,
    })
}

/// パイプを読み、行ごとにログと（あれば）コンソールの列へ渡す。番兵の行
/// （[`PIPE_END`]、[`StdioRedirect::finish`] が書く）が来たら、それより前の行を
/// 書き終えたログを書き出し（`sync_all`）、写しの送り口を手放して `done` で知らせる。
/// その後もパイプを読み続け、遅れて来た行はログへ書くだけにする（書き手を詰まらせ
/// ない）。書き込み側がすべて閉じられたら終わる（ふつうは閉じない）。
fn pump(
    read: RawHandle,
    log: &Mutex<ServerLog>,
    tee: Option<SyncSender<String>>,
    done: mpsc::Sender<()>,
) {
    let mut buffer = vec![0_u8; 8192];
    let mut pending: Vec<u8> = Vec::new();
    let mut tee = tee;
    let mut done = Some(done);
    let finish = |tee: &mut Option<SyncSender<String>>, done: &mut Option<mpsc::Sender<()>>| {
        if let Some(done) = done.take() {
            if let Ok(mut log) = log.lock() {
                log.flush();
            }
            // 写しのワーカーが残りを書き終えて終われるよう、送り口を手放す。
            tee.take();
            let _ = done.send(());
        }
    };
    let emit = |bytes: &[u8], tee: &Option<SyncSender<String>>| {
        let mut text = String::from_utf8_lossy(bytes).into_owned();
        while text.ends_with('\n') || text.ends_with('\r') {
            text.pop();
        }
        // 日付の切り替えで出る古いファイルの削除は、ロックを手放してから別の
        // スレッドで（上限の無い削除で、終了の最後の行の記録を待たせない）。
        let prune = log.lock().ok().and_then(|mut log| log.write_line(&text));
        if let Some(prune) = prune {
            crate::spawn_log_prune(prune);
        }
        if let Some(tee) = tee {
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
            if line == PIPE_END {
                finish(&mut tee, &mut done);
                continue;
            }
            emit(&line, &tee);
        }
    }
    if !pending.is_empty() {
        emit(&pending, &tee);
    }
    finish(&mut tee, &mut done);
}

/// 向け直しを終える: 標準ハンドルを元に戻し、パイプへ番兵を書く（モジュール doc
/// 「向け直し」。#598 監査 F4）。差し替えは Rust の標準出力・標準エラー出力の lock を
/// 持って行う: Rust の書き込みは lock の中で `GetStdHandle` → `WriteFile` を行うので、
/// lock を持っていた書き手は差し替えの前に書き終え（その行は番兵より前にある）、後の
/// 書き手は元のハンドルへ書く。番兵の書き込みは別のスレッドで行う（読み取り側が
/// 止まってパイプが一杯でも、呼び出し側は `done` を上限つきで待つだけにする）。
fn end_redirect(write: RawHandle, original_out: RawHandle, original_err: RawHandle) {
    use std::io::Write as _;
    {
        let mut stdout = std::io::stdout().lock();
        // 改行の無い `print!` の残りを、差し替えの前にパイプへ出し切る。
        let _ = stdout.flush();
        let _stderr = std::io::stderr().lock();
        // SAFETY: 元のハンドルに戻すだけ。
        unsafe {
            SetStdHandle(STD_OUTPUT_HANDLE, original_out.get());
            SetStdHandle(STD_ERROR_HANDLE, original_err.get());
        }
    }
    let spawned = std::thread::Builder::new()
        .name("server-log-end".into())
        .spawn(move || write_all(write, PIPE_END));
    if spawned.is_err() {
        write_all(write, PIPE_END);
    }
}

/// `bytes` をすべて `handle` へ書く（失敗したら諦める）。
fn write_all(handle: RawHandle, mut bytes: &[u8]) {
    while !bytes.is_empty() {
        let mut written = 0_u32;
        // SAFETY: バッファと長さは `bytes` のもの。同期 I/O。
        let ok = unsafe {
            WriteFile(
                handle.get(),
                bytes.as_ptr(),
                bytes.len() as u32,
                &mut written,
                std::ptr::null_mut(),
            )
        };
        if ok == FALSE || written == 0 {
            return;
        }
        bytes = &bytes[written as usize..];
    }
}

/// 列の行を元の標準エラー出力へ書く（コンソールなら UTF-16 で `WriteConsoleW`、
/// それ以外（パイプ・ファイル）なら UTF-8 で `WriteFile`）。失敗は捨てる。
fn console_writer(target: RawHandle) -> impl FnMut(&str) + Send + 'static {
    let mut mode = 0;
    // SAFETY: 出力のポインタは有効なローカル変数。
    let is_console = unsafe { GetConsoleMode(target.get(), &mut mode) } != FALSE;
    move |line: &str| {
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
    /// 1 行を**パイプを通さずに**ログファイルへ書いてディスクへ書き出し
    /// （`sync_all`）、コンソールの列にも入れる。戻った時点で行はディスクにある
    /// （#598: 後始末の本体の結果の記録を、結果を公開する前に確実に残すため。
    /// `eprintln!` はパイプを通り、読み取り側のスレッドが書くまでの間に OS が
    /// プロセスを終わらせうる）。向け直しを終えた後は標準エラー出力へ。
    pub fn write_through(&self, line: &str) {
        if self.finished.load(Ordering::SeqCst) {
            eprintln!("{line}");
            return;
        }
        let prune = self.log.lock().ok().and_then(|mut log| {
            let prune = log.write_line(line);
            log.flush();
            prune
        });
        if let Some(prune) = prune {
            crate::spawn_log_prune(prune);
        }
        if let Some(tee) = &self.tee {
            tee.send(line.to_string());
        }
    }

    /// 標準ハンドルを元に戻し、番兵より前の行をログに書き終え、コンソールへの写しを
    /// 書き終えるのを `wait` まで待つ（モジュール doc「向け直し」。2 回目以降の呼び出し
    /// も、最初の呼び出しが終わるまで戻らない）。パイプの書き込み側は閉じない。コンソールが止まっていて写しを書き
    /// 終えられなければ予算で諦める（ログには既に書いてある）。
    pub fn finish(&self, wait: Duration) {
        let deadline = std::time::Instant::now() + wait;
        let mut done = self.done.lock().unwrap_or_else(|p| p.into_inner());
        if self.finished.swap(true, Ordering::SeqCst) {
            return;
        }
        end_redirect(self.write, self.original_out, self.original_err);
        // ポンプが番兵より前の行をログに書き終える（そのとき自分の写しの送り口を手放す）。
        if let Some(rx) = done.take() {
            let _ = rx.recv_timeout(deadline.saturating_duration_since(std::time::Instant::now()));
        }
        // コンソールへの写しを書き終える（#598 の Copilot の指摘）。
        if let Some(tee) = &self.tee {
            tee.close_and_wait(deadline.saturating_duration_since(std::time::Instant::now()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 最後の行を列に入れた直後に閉じても、写しのワーカーが書き終えるまで待つ
    /// （書き先が遅くても）。反証: 送り口を手放さない・待たないと落ちる。
    #[test]
    fn close_and_wait_lets_the_tee_worker_write_the_last_line() {
        let written = Arc::new(Mutex::new(Vec::<String>::new()));
        let sink = written.clone();
        let (queue, pump_tx) = TeeQueue::spawn(move |line: &str| {
            std::thread::sleep(Duration::from_millis(200));
            sink.lock().unwrap().push(line.to_string());
        })
        .unwrap();
        drop(pump_tx); // ポンプは終わった
        queue.send("first".to_string());
        queue.send("終了の後始末が完了しました".to_string());
        assert!(
            queue.close_and_wait(Duration::from_secs(5)),
            "ワーカーが終わらない"
        );
        assert_eq!(
            *written.lock().unwrap(),
            vec![
                "first".to_string(),
                "終了の後始末が完了しました".to_string()
            ]
        );
        // 閉じた後に入れたものは捨てる。
        queue.send("late".to_string());
        assert_eq!(written.lock().unwrap().len(), 2);
    }

    /// #598 監査 F4: ポンプは書き込み側が閉じられなくても番兵で `done` を知らせ、
    /// 番兵より前の行はすべてログにあり、写しの送り口も手放している。番兵の後に来た
    /// 行でも書き手は詰まらない（ポンプが読み続ける）。
    #[test]
    fn the_pump_finishes_on_the_sentinel_without_closing_the_pipe() {
        let dir = tempfile::tempdir().unwrap();
        let log = Arc::new(Mutex::new(
            ServerLog::open(dir.path(), Arc::new(banto_tstore::SystemClock)).unwrap(),
        ));
        let mut read: HANDLE = std::ptr::null_mut();
        let mut write: HANDLE = std::ptr::null_mut();
        // SAFETY: 出力のポインタは有効なローカル変数。
        assert_ne!(
            unsafe { CreatePipe(&mut read, &mut write, std::ptr::null(), PIPE_BUFFER) },
            FALSE
        );
        let (read, write) = (RawHandle::new(read), RawHandle::new(write));
        let (tee_tx, tee_rx) = mpsc::sync_channel::<String>(TEE_QUEUE);
        let (done_tx, done_rx) = mpsc::channel();
        {
            let log = log.clone();
            std::thread::spawn(move || pump(read, &log, Some(tee_tx), done_tx));
        }
        write_all(write, "一行目\n".as_bytes());
        // 番兵と同じ書き込みで続く行は、番兵の後の行として扱う。
        let mut tail = "二行目\n".as_bytes().to_vec();
        tail.extend_from_slice(PIPE_END);
        tail.extend_from_slice(b"after\n");
        write_all(write, &tail);
        done_rx
            .recv_timeout(Duration::from_secs(10))
            .expect("番兵で done が来ない");
        let text = std::fs::read_to_string(log.lock().unwrap().current_path()).unwrap();
        assert!(text.contains("一行目") && text.contains("二行目"), "{text}");
        // 写しの送り口を手放している（受け手の列は番兵の前の 2 行で終わる）。
        assert_eq!(
            tee_rx.iter().collect::<Vec<_>>(),
            vec!["一行目".to_string(), "二行目".to_string()]
        );
        // 番兵の後も読み続けるので、パイプの大きさを超えて書いても詰まらない。
        let (wrote_tx, wrote_rx) = mpsc::channel();
        std::thread::spawn(move || {
            let mut line = vec![b'x'; 1023];
            line.push(b'\n');
            for _ in 0..256 {
                write_all(write, &line);
            }
            let _ = wrote_tx.send(());
        });
        wrote_rx
            .recv_timeout(Duration::from_secs(10))
            .expect("番兵の後の書き込みが詰まった");
    }

    /// 書き先が止まっている（コンソールの選択中など）と、予算を超えては待たない。
    #[test]
    fn close_and_wait_gives_up_after_the_budget_when_the_console_is_stuck() {
        let (queue, pump_tx) = TeeQueue::spawn(|_line: &str| {
            std::thread::sleep(Duration::from_secs(3600));
        })
        .unwrap();
        drop(pump_tx);
        queue.send("stuck".to_string());
        let started = std::time::Instant::now();
        assert!(!queue.close_and_wait(Duration::from_millis(200)));
        assert!(started.elapsed() < Duration::from_secs(2));
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
