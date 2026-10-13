//! `chronogazer-server`: 画面の無い本番用の ChronoGazer の記録エンジン（#392 段階 B
//! の B1-5、2026-10-13。設計は docs/chronogazer-service-design.md §3.1(b)）。
//!
//! B1 ではコンソールで動かす形だけ（引数なし）。同じ exe に Windows サービス
//! （`run-service`・`install`・`uninstall`）を足すのは B3。開発・E2E 用の
//! `banto-serve` とは別物で、あちらの既定（`0.0.0.0`・`./banto-dev.sqlite3`・
//! `BANTO_HUB_MEMORY_KEYS`）は持ち込まない。
//!
//! ```text
//! chronogazer-server.exe            コンソールで起動（Ctrl-C で停止）
//! chronogazer-server.exe --help     使い方
//! ```
//!
//! 環境変数は `CHRONOGAZER_ROOT`（データのルート。既定 `%ProgramData%\ChronoGazer`。
//! **絶対パスだけ**）だけ。待ち受けの port・LAN 公開は設定 DB の `server.*`、ログの
//! 保持日数は `log.retention_days`（既定 30）。判断（待ち受けの形・ルートの検証・
//! 失敗の案内・KeyStore・ログの名前と保持）は `chronogazer_core::server_host`。
//! このファイルは次を組むだけ:
//!
//! 1. ルートを解決して検証（相対パスは終了コード 3）。
//! 2. ログを開き（`<root>\logs\chronogazer-server-YYYY-MM-DD.log`）、標準出力・標準
//!    エラー出力をそこへ向け直してコンソールにも写す（Windows。`chronogazer_server/console.rs`）。
//! 3. コンソールの Ctrl を受ける（[`StopController`]）。
//! 4. `server_host::start`。失敗は理由と対処をコンソール（とログ）に出し、
//!    終了コード（A1 の使用中 = 2、それ以外 = 3）で終わる。対処は自動では行わない。
//! 5. 止める合図を待ち、後始末（`run\endpoint.json` を消す → エンジンの終了）を
//!    `banto_exit_once::ExitCleanupOnce` で 1 回だけ行う。Ctrl-C・Ctrl-Break は
//!    15 秒、コンソールのウィンドウを閉じた（`CTRL_CLOSE_EVENT`）・ログオフ・
//!    シャットダウンは 4 秒（OS が待つのは約 5 秒）で、ハンドラーのスレッドが
//!    後始末とログの書き出しの完了を待ってから戻る。
//!
//! 終了コード: 0 正常、1 後始末が予算内に終わらなかった・最終 flush に失敗、
//! 2 同じデータを別のプロセスが使用中、3 その他の起動の失敗、64 引数が違う。
//!
//! `--control-stdin`（自動テスト用。使い方には出さない）: 標準入力の行で止める
//! （`ctrl-c` = Ctrl-C と同じ経路、`close` = `CTRL_CLOSE_EVENT` のハンドラーと
//! 同じ関数を呼んだ後、OS と同じくプロセスを終わらせる。標準入力が閉じたら Ctrl-C と
//! 同じ）。**デバッグビルドだけ**（`#[cfg(debug_assertions)]`。統合テストは
//! debug ビルドで動く）。リリースビルドでは未知の引数として拒否する（終了コード 64）-
//! 標準入力をつないだ親プロセスなどから停止を指示できる入口を本番に残さないため
//! （デスクトップの `CHRONOGAZER_TEST_FAIL_TRAY` と同じ扱い）。

#[cfg(windows)]
#[path = "chronogazer_server/console.rs"]
mod console;

use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use banto_exit_once::{ExitCleanupOnce, ExitCleanupReport};
use chronogazer_core::paths::{resolve_data_root_from_env, DataLayout};
use chronogazer_core::server_host::{
    self, ServerLog, ServerOptions, ServerShutdownSummary, StartedServer, CLOSE_MARGIN, CLOSE_WAIT,
    EXIT_START_FAILED, EXIT_USAGE, INTERRUPT_BUDGET, SERVER_LABEL,
};

/// `CTRL_CLOSE_EVENT` でハンドラーから戻ったときに OS が使う終了コード
/// （`STATUS_CONTROL_C_EXIT`）。`--control-stdin` の `close` はこれで終わる。
const STATUS_CONTROL_C_EXIT: i32 = 0xC000_013A_u32 as i32;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let options = match parse_args(&args) {
        Ok(Some(options)) => options,
        Ok(None) => {
            print_usage();
            return;
        }
        Err(message) => {
            eprintln!("{SERVER_LABEL}: {message}");
            print_usage();
            std::process::exit(EXIT_USAGE);
        }
    };
    std::process::exit(run(options));
}

#[derive(Debug, Default, PartialEq, Eq)]
struct CliOptions {
    control_stdin: bool,
}

/// 引数の解釈。`Ok(None)` は使い方の表示だけ。
fn parse_args(args: &[String]) -> Result<Option<CliOptions>, String> {
    let mut options = CliOptions::default();
    for arg in args {
        match arg.as_str() {
            "-h" | "--help" => return Ok(None),
            other if accept_debug_only_arg(other, &mut options) => {}
            other => return Err(format!("不明な引数です: '{other}'")),
        }
    }
    Ok(Some(options))
}

/// テスト用の隠し引数（`--control-stdin`）。**デバッグビルドだけ**受け付ける
/// （モジュール doc）。リリースビルドでは常に `false` で、未知の引数として拒否される。
#[cfg(debug_assertions)]
fn accept_debug_only_arg(arg: &str, options: &mut CliOptions) -> bool {
    if arg == "--control-stdin" {
        options.control_stdin = true;
        return true;
    }
    false
}

#[cfg(not(debug_assertions))]
fn accept_debug_only_arg(_arg: &str, _options: &mut CliOptions) -> bool {
    false
}

fn print_usage() {
    eprintln!("使い方: chronogazer-server.exe");
    eprintln!("  コンソールで ChronoGazer の記録エンジンを動かします（Ctrl-C で停止）。");
    eprintln!(
        "  データ: 環境変数 {}（絶対パス）、既定 %ProgramData%\\ChronoGazer",
        chronogazer_core::paths::ROOT_ENV
    );
    eprintln!("  ログ: <データ>\\logs\\chronogazer-server-YYYY-MM-DD.log");
}

/// 止める合図の起点。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StopOrigin {
    /// Ctrl-C・Ctrl-Break（`--control-stdin` の `ctrl-c`・標準入力の終わり）。
    Interrupt,
    /// コンソールのウィンドウを閉じた。
    Close,
    /// ログオフ。
    Logoff,
    /// シャットダウン。
    Shutdown,
}

impl StopOrigin {
    fn label(self) -> &'static str {
        match self {
            Self::Interrupt => "Ctrl-C",
            Self::Close => "コンソールのウィンドウを閉じた",
            Self::Logoff => "ログオフ",
            Self::Shutdown => "シャットダウン",
        }
    }

    /// 後始末を待つ上限。
    fn budget(self) -> Duration {
        match self {
            Self::Interrupt => INTERRUPT_BUDGET,
            Self::Close | Self::Logoff | Self::Shutdown => CLOSE_WAIT,
        }
    }
}

/// エンジンの状態。
enum Phase {
    Starting,
    Ready {
        server: Arc<StartedServer>,
        runtime: tokio::runtime::Handle,
    },
    /// 起動に失敗した（後始末するものが無い）。
    Failed,
}

struct ControlState {
    phase: Phase,
    requested: Option<StopOrigin>,
}

/// 止める合図と後始末の 1 回化（モジュール doc の 5.）。main のスレッドと、
/// コンソールの Ctrl のハンドラーのスレッドが共有する。
struct StopController {
    state: Mutex<ControlState>,
    changed: Condvar,
    once: ExitCleanupOnce<ServerShutdownSummary, StopOrigin>,
    /// 標準出力・標準エラー出力の向け直しの後始末（無ければ何もしない）。
    flush: Box<dyn Fn(Duration) + Send + Sync>,
}

impl StopController {
    fn new(flush: Box<dyn Fn(Duration) + Send + Sync>) -> Self {
        Self {
            state: Mutex::new(ControlState {
                phase: Phase::Starting,
                requested: None,
            }),
            changed: Condvar::new(),
            once: ExitCleanupOnce::new(),
            flush,
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, ControlState> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// 止める合図を記録する（最初のものが残る）。
    fn request(&self, origin: StopOrigin) {
        let mut state = self.lock();
        state.requested.get_or_insert(origin);
        self.changed.notify_all();
    }

    fn set_phase(&self, phase: Phase) {
        self.lock().phase = phase;
        self.changed.notify_all();
    }

    /// main のスレッド: 合図が来るまで待つ。
    fn wait_request(&self) -> StopOrigin {
        let mut state = self.lock();
        loop {
            if let Some(origin) = state.requested {
                return origin;
            }
            state = self.changed.wait(state).unwrap_or_else(|p| p.into_inner());
        }
    }

    /// 後始末（`ExitCleanupOnce`。2 つ目以降の呼び出しは先行の完了を自分の予算まで
    /// 待つ）。エンジンがまだ起動中なら、起動の終わりを `budget` の中で待つ。
    /// 後始末するものが無ければ（起動に失敗した・待ちきれない）`None`。
    fn cleanup(
        &self,
        origin: StopOrigin,
        budget: Duration,
    ) -> Option<ExitCleanupReport<ServerShutdownSummary, StopOrigin>> {
        let deadline = Instant::now() + budget;
        let (server, runtime) = {
            let mut state = self.lock();
            loop {
                match &state.phase {
                    Phase::Ready { server, runtime } => break (server.clone(), runtime.clone()),
                    Phase::Failed => return None,
                    Phase::Starting => {
                        let now = Instant::now();
                        if now >= deadline {
                            return None;
                        }
                        state = self
                            .changed
                            .wait_timeout(state, deadline - now)
                            .unwrap_or_else(|p| p.into_inner())
                            .0;
                    }
                }
            }
        };
        let remaining = deadline.saturating_duration_since(Instant::now());
        Some(
            self.once
                .run_or_wait(origin, remaining, move |work_deadline| {
                    let engine_budget = work_deadline
                        .saturating_duration_since(Instant::now())
                        .saturating_sub(CLOSE_MARGIN);
                    runtime.block_on(server.shutdown(engine_budget))
                }),
        )
    }

    /// 後始末の結果を出す（本体を走らせた呼び出しだけが結果を出す。banto-hub・
    /// デスクトップと同じ作法）。終了コードを返す。
    fn report(
        &self,
        origin: StopOrigin,
        report: Option<ExitCleanupReport<ServerShutdownSummary, StopOrigin>>,
    ) -> i32 {
        match report {
            None => {
                eprintln!(
                    "{SERVER_LABEL}: 止めるエンジンがありません（{}）",
                    origin.label()
                );
                EXIT_START_FAILED
            }
            Some(ExitCleanupReport::RanWork(outcome)) => match outcome {
                banto_exit_once::ExitCleanupOutcome::Finished(summary) => {
                    describe_summary(origin, &summary);
                    summary.exit_code()
                }
                banto_exit_once::ExitCleanupOutcome::TimedOut => {
                    eprintln!(
                        "{SERVER_LABEL}: 終了の後始末が{}秒以内に終わりませんでした（{}）。最後の数秒の記録が失われた可能性があります。",
                        origin.budget().as_secs(),
                        origin.label()
                    );
                    server_host::EXIT_SHUTDOWN_INCOMPLETE
                }
            },
            Some(ExitCleanupReport::ReceivedPrior { result, .. }) => result.exit_code(),
            Some(ExitCleanupReport::PriorExpired { origin: prior }) => {
                eprintln!(
                    "{SERVER_LABEL}: 終了の後始末（{}で開始）が期限内に終わりませんでした。",
                    prior.label()
                );
                server_host::EXIT_SHUTDOWN_INCOMPLETE
            }
            Some(ExitCleanupReport::OwnBudgetExpired { origin: prior }) => {
                eprintln!(
                    "{SERVER_LABEL}: 終了の後始末（{}で開始）を{}秒まで待ちましたが終わりませんでした（{}）。",
                    prior.label(),
                    origin.budget().as_secs(),
                    origin.label()
                );
                server_host::EXIT_SHUTDOWN_INCOMPLETE
            }
        }
    }

    /// コンソールのウィンドウを閉じた・ログオフ・シャットダウン: このスレッド
    /// （Ctrl のハンドラー）で後始末とログの書き出しの完了を待つ。戻るとプロセスが
    /// 終わる（モジュール doc）。
    fn on_close(&self, origin: StopOrigin) -> i32 {
        let started = Instant::now();
        self.request(origin);
        let report = self.cleanup(origin, origin.budget());
        let code = self.report(origin, report);
        eprintln!(
            "{SERVER_LABEL}: 終了します（{}、終了コード {code}）",
            origin.label()
        );
        // 書き出しは OS の猶予（約 5 秒）の残りで。
        let flush_wait = Duration::from_millis(4_800).saturating_sub(started.elapsed());
        (self.flush)(flush_wait.max(Duration::from_millis(200)));
        code
    }
}

fn describe_summary(origin: StopOrigin, summary: &ServerShutdownSummary) {
    if let Some(err) = &summary.report.collect_stop_error {
        eprintln!("{SERVER_LABEL}: 最終の書き出しに失敗しました: {err}");
    }
    if summary.report.timed_out {
        // エンジンが自分で 1 行出している（`engine::RunningEngine::shutdown`）。
        return;
    }
    println!(
        "{SERVER_LABEL}: 終了の後始末が完了しました（{}）",
        origin.label()
    );
}

fn run(options: CliOptions) -> i32 {
    // 1. ルート。
    let root = resolve_data_root_from_env();
    if let Err(message) = server_host::validate_root(&root) {
        eprintln!("{SERVER_LABEL}: {message}");
        return EXIT_START_FAILED;
    }
    let layout = DataLayout::new(root);

    // 2. ログ（Windows だけ。標準ハンドルの向け直しは Windows の API）。
    let (log, flush) = open_log(&layout);

    // 3. 止める合図。
    let controller = Arc::new(StopController::new(flush));
    install_stop_sources(&controller, &options);

    println!(
        "{SERVER_LABEL}: 起動します（版 {}、pid {}）",
        env!("CARGO_PKG_VERSION"),
        std::process::id()
    );
    println!(
        "{SERVER_LABEL}: データのルート: {}",
        layout.root().display()
    );
    match &log {
        Some(log) => println!(
            "{SERVER_LABEL}: ログ: {}",
            log.lock()
                .map(|log| log.current_path())
                .unwrap_or_else(|_| PathBuf::from("?"))
                .display()
        ),
        None => println!("{SERVER_LABEL}: ログファイルには書きません（コンソールだけ）"),
    }

    // 4. 起動。
    let (key_store, key_store_error) = server_host::open_key_store(&layout);
    if let Some(reason) = key_store_error {
        eprintln!(
            "{SERVER_LABEL}: Hub の API キーの保存先（{}）を使えません。Hub 経由の収集は動きません（PLC の収集は続けます）: {reason}",
            layout.hub_keys_dir().display()
        );
    }
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(err) => {
            eprintln!("{SERVER_LABEL}: 非同期ランタイムを作れませんでした: {err}");
            controller.set_phase(Phase::Failed);
            (controller.flush)(Duration::from_secs(2));
            return EXIT_START_FAILED;
        }
    };
    let started = runtime.block_on(server_host::start(ServerOptions {
        layout: layout.clone(),
        key_store,
    }));
    let server = match started {
        Ok(server) => Arc::new(server),
        Err(failure) => {
            eprintln!("{SERVER_LABEL}: 起動できませんでした: {}", failure.message);
            for line in &failure.guidance {
                eprintln!("{SERVER_LABEL}:   {line}");
            }
            let code = failure.exit_code();
            eprintln!("{SERVER_LABEL}: 終了します（終了コード {code}）");
            controller.set_phase(Phase::Failed);
            (controller.flush)(Duration::from_secs(2));
            return code;
        }
    };
    if let Some(log) = &log {
        if let Ok(mut log) = log.lock() {
            log.set_retention(server.log_retention_days());
        }
    }
    runtime.block_on(announce(&server));
    controller.set_phase(Phase::Ready {
        server: server.clone(),
        runtime: runtime.handle().clone(),
    });

    // 5. 止める合図を待って後始末。
    let origin = controller.wait_request();
    if origin != StopOrigin::Interrupt {
        // ウィンドウを閉じた・ログオフ・シャットダウン: 後始末・結果の表示・ログの
        // 書き出しは Ctrl のハンドラーのスレッド（`StopController::on_close`）が行い、
        // 戻るとプロセスが終わる。こちらが先に書き出しを閉じたり `exit` したりして、
        // ハンドラーの最後の行を落とさないよう、ここでは何もせず待つ。
        loop {
            std::thread::park();
        }
    }
    println!("{SERVER_LABEL}: 停止します（{}）", origin.label());
    let report = controller.cleanup(origin, origin.budget());
    let code = controller.report(origin, report);
    println!("{SERVER_LABEL}: 終了しました（終了コード {code}）");
    (controller.flush)(Duration::from_secs(2));
    // ランタイムは drop しない（止めたエンジンの残りのタスクを待たない）。A1 の
    // 排他はプロセスの終わりで OS が返す。
    std::mem::forget(runtime);
    code
}

async fn announce(server: &StartedServer) {
    let engine = server.engine();
    if let Some(addr) = engine.loopback_addr().await {
        println!(
            "{SERVER_LABEL}: 待ち受け（この PC から）: http://127.0.0.1:{}/",
            addr.port()
        );
        if server.plan().loopback.bind != "127.0.0.1" {
            for url in chronogazer_core::listening_urls(addr) {
                println!("{SERVER_LABEL}: 待ち受け（LAN）: {url}");
            }
        }
    }
    if let Some(addr) = engine.server_slot().local_addr().await {
        for url in chronogazer_core::listening_urls(addr) {
            println!("{SERVER_LABEL}: 待ち受け（LAN）: {url}");
        }
    }
    if let Some(err) = engine.listen_error() {
        eprintln!(
            "{SERVER_LABEL}: LAN の待ち受けを開けませんでした（この PC からは使えます）: {err}"
        );
    }
    if server.plan().exposes_lan() {
        println!(
            "{SERVER_LABEL}: LAN に公開しているため、最初の管理者の作成（/api/auth/setup）は閉じています"
        );
    }
    if let Some(err) = server.endpoint_error() {
        eprintln!("{SERVER_LABEL}: {err}");
    }
    println!("{SERVER_LABEL}: Ctrl-C で停止します");
}

type Flush = Box<dyn Fn(Duration) + Send + Sync>;

#[cfg(windows)]
fn open_log(layout: &DataLayout) -> (Option<Arc<Mutex<ServerLog>>>, Flush) {
    let log = match ServerLog::open(layout.logs_dir(), Arc::new(banto_tstore::SystemClock)) {
        Ok(log) => Arc::new(Mutex::new(log)),
        Err(err) => {
            eprintln!(
                "{SERVER_LABEL}: ログ（{}）を開けませんでした。コンソールだけに出します: {err}",
                layout.logs_dir().display()
            );
            return (None, Box::new(|_| {}));
        }
    };
    match console::redirect_stdio(log.clone()) {
        Ok(redirect) => {
            let redirect = Arc::new(redirect);
            (Some(log), Box::new(move |wait| redirect.finish(wait)))
        }
        Err(err) => {
            eprintln!("{SERVER_LABEL}: 標準出力をログへ向け直せませんでした: {err}");
            (None, Box::new(|_| {}))
        }
    }
}

#[cfg(not(windows))]
fn open_log(_layout: &DataLayout) -> (Option<Arc<Mutex<ServerLog>>>, Flush) {
    (None, Box::new(|_| {}))
}

fn install_stop_sources(controller: &Arc<StopController>, options: &CliOptions) {
    #[cfg(windows)]
    {
        let handler = controller.clone();
        let installed = console::install_ctrl_handler(Box::new(move |kind| match kind {
            console::CtrlKind::Interrupt => handler.request(StopOrigin::Interrupt),
            console::CtrlKind::Close => {
                handler.on_close(StopOrigin::Close);
            }
            console::CtrlKind::Logoff => {
                handler.on_close(StopOrigin::Logoff);
            }
            console::CtrlKind::Shutdown => {
                handler.on_close(StopOrigin::Shutdown);
            }
        }));
        if let Err(err) = installed {
            eprintln!("{SERVER_LABEL}: Ctrl のハンドラーを登録できませんでした: {err}");
        }
    }
    #[cfg(not(windows))]
    {
        let handler = controller.clone();
        std::thread::spawn(move || {
            let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            else {
                return;
            };
            if runtime.block_on(tokio::signal::ctrl_c()).is_ok() {
                handler.request(StopOrigin::Interrupt);
            }
        });
    }
    if options.control_stdin {
        let handler = controller.clone();
        std::thread::spawn(move || {
            let mut line = String::new();
            loop {
                line.clear();
                match std::io::stdin().read_line(&mut line) {
                    Ok(0) | Err(_) => {
                        handler.request(StopOrigin::Interrupt);
                        return;
                    }
                    Ok(_) => match line.trim() {
                        "ctrl-c" => handler.request(StopOrigin::Interrupt),
                        "close" => {
                            handler.on_close(StopOrigin::Close);
                            // OS はハンドラーから戻るとプロセスを終わらせる。
                            std::process::exit(STATUS_CONTROL_C_EXIT);
                        }
                        _ => {}
                    },
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn parse_args_accepts_nothing_help_and_the_hidden_control_flag() {
        assert_eq!(parse_args(&[]), Ok(Some(CliOptions::default())));
        assert_eq!(parse_args(&args(&["--help"])), Ok(None));
        assert_eq!(parse_args(&args(&["-h"])), Ok(None));
        // テスト用の隠し引数はデバッグビルドだけ受け、リリースビルドでは拒否する。
        #[cfg(debug_assertions)]
        assert_eq!(
            parse_args(&args(&["--control-stdin"])),
            Ok(Some(CliOptions {
                control_stdin: true
            }))
        );
        #[cfg(not(debug_assertions))]
        assert!(parse_args(&args(&["--control-stdin"])).is_err());
        // B3 の `run-service` などはまだ無い。
        for unknown in ["run-service", "install", "--port", "x"] {
            assert!(parse_args(&args(&[unknown])).is_err(), "{unknown}");
        }
    }

    #[test]
    fn close_like_origins_use_the_short_budget() {
        assert_eq!(StopOrigin::Interrupt.budget(), Duration::from_secs(15));
        for origin in [StopOrigin::Close, StopOrigin::Logoff, StopOrigin::Shutdown] {
            assert_eq!(origin.budget(), Duration::from_secs(4));
            // OS の猶予（約 5 秒）に収まる。
            assert!(origin.budget() < Duration::from_secs(5));
        }
    }

    /// 起動中に閉じられたら、起動の終わりを予算の中で待ち、起動に失敗していれば
    /// 後始末するものが無い（`None`）。待ちは予算で切れる。
    #[test]
    fn cleanup_waits_for_the_start_within_its_budget() {
        let controller = Arc::new(StopController::new(Box::new(|_| {})));
        let started = Instant::now();
        assert!(controller
            .cleanup(StopOrigin::Close, Duration::from_millis(200))
            .is_none());
        assert!(started.elapsed() >= Duration::from_millis(200));

        let waiter = {
            let controller = controller.clone();
            std::thread::spawn(move || {
                controller.cleanup(StopOrigin::Close, Duration::from_secs(10))
            })
        };
        std::thread::sleep(Duration::from_millis(50));
        controller.set_phase(Phase::Failed);
        assert!(waiter.join().unwrap().is_none());
    }

    /// 合図は最初のものが残り、待っている main を起こす。
    #[test]
    fn the_first_request_wins_and_wakes_the_waiter() {
        let controller = Arc::new(StopController::new(Box::new(|_| {})));
        let waiter = {
            let controller = controller.clone();
            std::thread::spawn(move || controller.wait_request())
        };
        std::thread::sleep(Duration::from_millis(50));
        controller.request(StopOrigin::Close);
        controller.request(StopOrigin::Interrupt);
        assert_eq!(waiter.join().unwrap(), StopOrigin::Close);
        assert_eq!(controller.wait_request(), StopOrigin::Close);
    }
}
