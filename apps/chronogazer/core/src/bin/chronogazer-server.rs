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
//! 2. コンソールの Ctrl を受ける（[`StopController`]）。登録できなければ何も作らずに
//!    終了コード 3（#598）。
//! 3. ルートを作って A1 の排他を取る（`server_host::lock`）。取れなければルートの下に
//!    何も作らず、理由はコンソールだけに出して終了コード 2。
//! 4. ログを開き（`<root>\logs\chronogazer-server-YYYY-MM-DD.log`）、標準出力・標準
//!    エラー出力をそこへ向け直してコンソールにも写す（Windows。`chronogazer_server/console.rs`）。
//!    **リストア予約の適用と DB の初期化より前**（#598 のオーナーレビュー P2: DB を
//!    開けない理由と対処、リストア予約の警告を日次ログに残す）。
//! 5. リストア予約の適用 → DB の初期化（`LockedServer::open`）→ Hub のキーの保存先 →
//!    エンジン（`OpenedServer::start`）。失敗は理由と対処をコンソールとログに出し、
//!    終了コード 3 で終わる。対処は自動では行わない。**理由・対処を出してログを
//!    書き出し終えてから**起動の失敗を知らせる（起動中に閉じたウィンドウのハンドラーが
//!    待っている。#598 監査 F1）。段の間（DB の初期化の前・エンジンの開始の前・開始の
//!    後）で止める合図を見て、来ていれば起動を中止する（終了コード 3。開始の後なら
//!    案内を出さずに後始末へ。#598 監査 F3）。段の途中（DB を開いている最中など）は
//!    止めない。
//! 6. 止める合図を待ち、後始末（`run\endpoint.json` を消す → エンジンの終了）を
//!    `banto_exit_once::ExitCleanupOnce` で 1 回だけ行う。Ctrl-C・Ctrl-Break は
//!    15 秒、コンソールのウィンドウを閉じた（`CTRL_CLOSE_EVENT`）・ログオフ・
//!    シャットダウンは 4 秒（OS が待つのは約 5 秒）で、ハンドラーのスレッドが
//!    後始末とログの書き出しの完了を待ち、**自分でプロセスを終わらせる**
//!    （[`close_and_exit`]。ハンドラーから戻ったときに OS が終わらせるのを当てに
//!    しない。#598 監査 F5）。
//!
//! 終了コード: 0 正常、1 後始末が予算内に終わらなかった・最終 flush に失敗、
//! 2 同じデータを別のプロセスが使用中、3 その他の起動の失敗、64 引数が違う。
//!
//! `--control-stdin`（自動テスト用。使い方には出さない）: 標準入力の行で止める
//! （`ctrl-c` = Ctrl-C と同じ経路、`close` = `CTRL_CLOSE_EVENT` のハンドラーと
//! 同じ関数 [`close_and_exit`] を呼ぶ。標準入力が閉じたら Ctrl-C と同じ）。**デバッグビルドだけ**（`#[cfg(debug_assertions)]`。統合テストは
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
    self, ServerLog, ServerShutdownSummary, StartedServer, CLOSE_MARGIN, CLOSE_WAIT,
    EXIT_START_FAILED, EXIT_USAGE, INTERRUPT_BUDGET, SERVER_LABEL,
};

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
    /// 起動に失敗した・中止した（後始末するものが無い）。`code` は main が終わる
    /// ときの終了コード（close 系のハンドラーも同じ値で終わる）。
    Failed {
        code: i32,
    },
}

/// [`StopController::cleanup`] で後始末するエンジンが無い理由。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NoEngine {
    /// 起動に失敗した・起動を中止した（main が理由を出し、ログを書き出し終えている）。
    StartFailed { code: i32 },
    /// 予算の中で起動が終わらなかった（#598 監査 F8）。main はまだ起動の途中で、
    /// 起動が終わったら [`StopController::set_ready`] がそれを知らせる。
    StartAbandoned,
}

struct ControlState {
    phase: Phase,
    requested: Option<StopOrigin>,
    /// 走っている close 系のハンドラー（[`StopController::on_close`]）の数。
    /// Ctrl-C の後に close 系の事象が来たとき、main がその終わりを待たずに
    /// `exit` しないため（#598 の Copilot の指摘）。
    closing: usize,
    /// main が終わりに入った（ログを書き出して `exit` する）。この後の close 系の
    /// ハンドラーは何もせずに待つ（main の書き出しを邪魔しない）。
    exiting: bool,
    /// close 系のハンドラーが起動の終わりを待ちきれずに諦めた（#598 監査 F3・F8）。
    /// main は起動が終わっても `Ready` にせず、すぐに止める（[`StopController::set_ready`]）。
    start_abandoned: bool,
}

/// 止める合図と後始末の 1 回化（モジュール doc の 5.）。main のスレッドと、
/// コンソールの Ctrl のハンドラーのスレッドが共有する。
struct StopController {
    state: Mutex<ControlState>,
    changed: Condvar,
    once: ExitCleanupOnce<ServerShutdownSummary, StopOrigin>,
    /// 標準出力・標準エラー出力の向け直しの後始末（無ければ何もしない）。ログは
    /// A1 の排他を取った後に開くので、後から入れる（[`StopController::set_flush`]）。
    flush: Mutex<Option<Flush>>,
    /// 後始末の本体の結果を、戻る前にディスクまで書く記録先（無ければ標準エラー
    /// 出力。[`StopController::set_record`]）。
    record: Mutex<Option<LineSink>>,
}

/// 1 行を記録する口（戻った時点で記録が済んでいること）。
type LineSink = Arc<dyn Fn(&str) + Send + Sync>;

/// 後始末の本体の結果の記録（本体を走らせた経路 `origin` の名で、本体について
/// 1 回だけ）。最終の書き出しの失敗と、完了（予算切れでなければ）。予算切れは
/// エンジンが自分で 1 行出している（`engine::RunningEngine::shutdown`）。
fn body_log_lines(origin: StopOrigin, summary: &ServerShutdownSummary) -> Vec<String> {
    let mut lines = Vec::new();
    if let Some(err) = &summary.report.collect_stop_error {
        lines.push(format!(
            "{SERVER_LABEL}: 最終の書き出しに失敗しました: {err}"
        ));
    }
    if !summary.report.timed_out {
        lines.push(format!(
            "{SERVER_LABEL}: 終了の後始末が完了しました（{}）",
            origin.label()
        ));
    }
    lines
}

/// 後始末を 1 回だけ走らせ、2 つ目以降には完了を待たせる（`ExitCleanupOnce`）。
/// **本体の結果の記録は、本体を走らせたワーカーの中で、結果を公開する前に書き終える**
/// （#598 のレビュー。banto-hub の `run_exit_cleanup` と同じ形、#587）: 後から来た
/// 待ち手（例: Ctrl-C の後始末の途中で閉じたウィンドウのハンドラー）は結果の
/// 公開と同時に起きて戻り、Windows はハンドラーの戻りでプロセスを終わらせる
/// ので、公開の後に記録すると完了の行が残らないことがある。待ちの打ち切りの記録は
/// 各呼び出しが出す（[`StopController::report`]）。
fn run_cleanup(
    once: &ExitCleanupOnce<ServerShutdownSummary, StopOrigin>,
    origin: StopOrigin,
    budget: Duration,
    work: impl FnOnce(Instant) -> ServerShutdownSummary + Send + 'static,
    record: LineSink,
) -> ExitCleanupReport<ServerShutdownSummary, StopOrigin> {
    once.run_or_wait(origin, budget, move |deadline| {
        let summary = work(deadline);
        for line in body_log_lines(origin, &summary) {
            record(&line);
        }
        summary
    })
}

impl StopController {
    fn new() -> Self {
        Self {
            state: Mutex::new(ControlState {
                phase: Phase::Starting,
                requested: None,
                closing: 0,
                exiting: false,
                start_abandoned: false,
            }),
            changed: Condvar::new(),
            once: ExitCleanupOnce::new(),
            flush: Mutex::new(None),
            record: Mutex::new(None),
        }
    }

    fn set_record(&self, record: LineSink) {
        *self.record.lock().unwrap_or_else(|p| p.into_inner()) = Some(record);
    }

    fn record_sink(&self) -> LineSink {
        self.record
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
            .unwrap_or_else(|| Arc::new(|line: &str| eprintln!("{line}")))
    }

    fn set_flush(&self, flush: Flush) {
        *self.flush.lock().unwrap_or_else(|p| p.into_inner()) = Some(flush);
    }

    /// ログの書き出しを `wait` まで待つ（ログを開いていなければ何もしない）。
    fn flush(&self, wait: Duration) {
        let flush = self.flush.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(flush) = flush.as_ref() {
            flush(wait);
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

    /// 止める合図が既に来ているか（#598 監査 F3: main が起動の段の間で見て、起動を
    /// 中止する）。
    fn stop_requested(&self) -> Option<StopOrigin> {
        self.lock().requested
    }

    /// 起動の成功を知らせる（`Ready`）。close 系のハンドラーが起動の終わりを待ちきれずに
    /// 諦めていたら（[`NoEngine::StartAbandoned`]）`Ready` にせず `false`: そのハンドラーは
    /// もう後始末をしないので、main がすぐに止める。判定と設定を 1 つの lock の中で行い、
    /// 諦めと `Ready` の行き違いを作らない。
    fn set_ready(&self, server: Arc<StartedServer>, runtime: tokio::runtime::Handle) -> bool {
        let mut state = self.lock();
        if state.start_abandoned {
            return false;
        }
        state.phase = Phase::Ready { server, runtime };
        self.changed.notify_all();
        true
    }

    /// close 系のハンドラーの始まり。main が既に終わりに入っていれば `false`。
    fn begin_close(&self) -> bool {
        let mut state = self.lock();
        if state.exiting {
            return false;
        }
        state.closing += 1;
        true
    }

    /// main の終わり: close 系のハンドラーが走っていなければ終わりに入る（`true`。
    /// 以後のハンドラーは何もしない）。走っていれば `false` で、終わり（結果の表示・
    /// ログの書き出し・プロセスの終了）はハンドラーに任せる。
    fn claim_exit(&self) -> bool {
        let mut state = self.lock();
        if state.closing > 0 {
            return false;
        }
        state.exiting = true;
        true
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
    /// 後始末するものが無ければ（起動に失敗した・中止した・待ちきれない）`Err`。
    /// 待ちきれなかったときは諦めた印を立てる（[`StopController::set_ready`]）。
    fn cleanup(
        &self,
        origin: StopOrigin,
        budget: Duration,
    ) -> Result<ExitCleanupReport<ServerShutdownSummary, StopOrigin>, NoEngine> {
        let deadline = Instant::now() + budget;
        let (server, runtime) = {
            let mut state = self.lock();
            loop {
                match &state.phase {
                    Phase::Ready { server, runtime } => break (server.clone(), runtime.clone()),
                    Phase::Failed { code } => return Err(NoEngine::StartFailed { code: *code }),
                    Phase::Starting => {
                        let now = Instant::now();
                        if now >= deadline {
                            state.start_abandoned = true;
                            return Err(NoEngine::StartAbandoned);
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
        Ok(run_cleanup(
            &self.once,
            origin,
            remaining,
            move |work_deadline| {
                let engine_budget = work_deadline
                    .saturating_duration_since(Instant::now())
                    .saturating_sub(CLOSE_MARGIN);
                runtime.block_on(server.shutdown(engine_budget))
            },
            self.record_sink(),
        ))
    }

    /// 後始末の結果を出す（本体を走らせた呼び出しだけが結果を出す。banto-hub・
    /// デスクトップと同じ作法）。終了コードを返す。
    fn report(
        &self,
        origin: StopOrigin,
        report: Result<ExitCleanupReport<ServerShutdownSummary, StopOrigin>, NoEngine>,
    ) -> i32 {
        match report {
            // 理由と対処は main が出し、ログも書き出し終えている（`finish_start_failure`）。
            Err(NoEngine::StartFailed { code }) => {
                eprintln!(
                    "{SERVER_LABEL}: 起動に失敗した・中止したため、止めるエンジンはありません（{}）",
                    origin.label()
                );
                code
            }
            // #598 監査 F8: 起動が予算の中で終わらなかった。最終の書き出しはしていない。
            Err(NoEngine::StartAbandoned) => {
                eprintln!(
                    "{SERVER_LABEL}: 起動が終わる前に止める合図を受け、{}秒の中で起動が終わらなかったため、起動の途中のまま終わります（{}）",
                    origin.budget().as_secs(),
                    origin.label()
                );
                server_host::EXIT_SHUTDOWN_INCOMPLETE
            }
            Ok(ExitCleanupReport::RanWork(outcome)) => match outcome {
                // 本体の結果はワーカーが公開の前に記録済み（`run_cleanup`）。
                banto_exit_once::ExitCleanupOutcome::Finished(summary) => summary.exit_code(),
                banto_exit_once::ExitCleanupOutcome::TimedOut => {
                    eprintln!(
                        "{SERVER_LABEL}: 終了の後始末が{}秒以内に終わりませんでした（{}）。最後の数秒の記録が失われた可能性があります。",
                        origin.budget().as_secs(),
                        origin.label()
                    );
                    server_host::EXIT_SHUTDOWN_INCOMPLETE
                }
            },
            Ok(ExitCleanupReport::ReceivedPrior { result, .. }) => result.exit_code(),
            Ok(ExitCleanupReport::PriorExpired { origin: prior }) => {
                eprintln!(
                    "{SERVER_LABEL}: 終了の後始末（{}で開始）が期限内に終わりませんでした。",
                    prior.label()
                );
                server_host::EXIT_SHUTDOWN_INCOMPLETE
            }
            Ok(ExitCleanupReport::OwnBudgetExpired { origin: prior }) => {
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
    /// （Ctrl のハンドラー）で後始末とログの書き出しの完了を待ち、終了コードを返す
    /// （呼び出し側 [`close_and_exit`] がそのコードでプロセスを終わらせる）。
    fn on_close(&self, origin: StopOrigin) -> i32 {
        let started = Instant::now();
        if !self.begin_close() {
            // main が既に終わり（ログの書き出しと `exit`）に入っている。戻ると OS が
            // プロセスを終わらせ、main の書き出しを切るので、main の `exit` を待つ。
            loop {
                std::thread::park();
            }
        }
        self.request(origin);
        let report = self.cleanup(origin, origin.budget());
        let code = self.report(origin, report);
        eprintln!(
            "{SERVER_LABEL}: 終了します（{}、終了コード {code}）",
            origin.label()
        );
        // 書き出しは OS の猶予（約 5 秒）の残りで。
        let flush_wait = Duration::from_millis(4_800).saturating_sub(started.elapsed());
        self.flush(flush_wait.max(Duration::from_millis(200)));
        code
    }
}

/// close 系の事象（ウィンドウを閉じた・ログオフ・シャットダウン）の受け口:
/// [`StopController::on_close`] の後に、その終了コードで**自分で**プロセスを終わらせる
/// （#598 監査 F5）。Windows はハンドラーから戻るとプロセスを終わらせるが、ログオフ・
/// シャットダウンで終わらせる時機は OS 次第で、戻った後もプロセスが残ると main は
/// 待ったまま（`park`）になる。終了コードも OS の `STATUS_CONTROL_C_EXIT` ではなく
/// 後始末の結果にする。本物のハンドラーと `--control-stdin` の `close` が同じ関数を通る。
/// main が既に終わりに入っていれば、`on_close` は main の `exit` を待って戻らない。
fn close_and_exit(controller: &StopController, origin: StopOrigin) -> ! {
    let code = controller.on_close(origin);
    std::process::exit(code)
}

fn run(options: CliOptions) -> i32 {
    // 1. ルート。
    let root = resolve_data_root_from_env();
    if let Err(message) = server_host::validate_root(&root) {
        eprintln!("{SERVER_LABEL}: {message}");
        return EXIT_START_FAILED;
    }
    let layout = DataLayout::new(root);

    // 2. 止める合図（ルートの下に何も作らない。登録できなければ起動しない）。
    let controller = Arc::new(StopController::new());
    if let Err(err) = install_stop_sources(&controller, &options) {
        eprintln!(
            "{SERVER_LABEL}: Ctrl のハンドラーを登録できませんでした。Ctrl-C やコンソールを閉じたときに後始末（最終の書き出し）を通せないため、起動しません: {err}"
        );
        return EXIT_START_FAILED;
    }

    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(err) => {
            eprintln!("{SERVER_LABEL}: 非同期ランタイムを作れませんでした: {err}");
            return EXIT_START_FAILED;
        }
    };

    // 3. A1 の排他だけ（ルートの下に作るのはここから。排他を取れなければ何も
    // 作らない - ログもまだ開いていないので、理由はコンソールだけに出す）。
    let locked = match server_host::lock(layout.clone()) {
        Ok(locked) => locked,
        Err(failure) => return report_start_failure(&controller, &failure),
    };

    // 4. ログ（排他を取った後・リストア予約の適用と DB の初期化の前。#598 の
    // オーナーレビュー P2: DB を開けない理由と対処、リストア予約の警告を日次ログに
    // 残す。Windows だけ - 標準ハンドルの向け直しは Windows の API）。
    let (log, flush, record) = match open_log(&layout) {
        Ok(opened) => opened,
        Err(reason) => {
            // 日次ログはこの bin の唯一の永続の診断（サービスではコンソールも無い）。
            // 向け直せないまま動かすと起動・停止・失敗の記録が残らないので、起動を
            // 止める（Ctrl のハンドラーの登録の失敗と同じ扱い。#598 の Copilot の指摘）。
            // 理由はログファイルへ直接（`open_log` が書いた）とコンソールに出す。
            eprintln!("{SERVER_LABEL}: {reason}");
            return finish_start_failure(&controller, EXIT_START_FAILED);
        }
    };
    controller.set_flush(flush);
    if let Some(record) = record {
        controller.set_record(record);
    }
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

    // 起動の段の間で止める合図を見る（#598 監査 F3）: 合図の後に DB の初期化・
    // 待ち受け・収集の開始・`endpoint.json` まで進めない。段の途中（DB を開いている
    // 最中など）は止めない（DB の操作を途中で落とさない。checklist §5）。
    if let Some(origin) = controller.stop_requested() {
        return abort_start(&controller, origin);
    }

    // 5. リストア予約の適用 → DB の初期化（失敗はログにも残る）。
    #[cfg(debug_assertions)]
    test_slow_open(&controller);
    let opened = match runtime.block_on(locked.open()) {
        Ok(opened) => opened,
        Err(failure) => return report_start_failure(&controller, &failure),
    };

    // 6. Hub のキーの保存先（排他を取った後）とエンジン。
    let (key_store, key_store_error) = server_host::open_key_store(&layout);
    if let Some(reason) = key_store_error {
        eprintln!(
            "{SERVER_LABEL}: Hub の API キーの保存先（{}）を使えません。Hub 経由の収集は動きません（PLC の収集は続けます）: {reason}",
            layout.hub_keys_dir().display()
        );
    }
    if let Some(origin) = controller.stop_requested() {
        runtime.block_on(opened.close());
        return abort_start(&controller, origin);
    }
    let server = match runtime.block_on(opened.start(key_store)) {
        Ok(server) => Arc::new(server),
        Err(failure) => return report_start_failure(&controller, &failure),
    };
    if let Some(log) = &log {
        if let Ok(mut log) = log.lock() {
            log.set_retention(server.log_retention_days());
        }
    }
    // 起動の最中に合図が来ていたら、待ち受けの案内を出さずにすぐ後始末へ進む
    // （Ctrl-C なら下の `wait_request` がすぐ返り、close 系なら待っているハンドラーが
    // 後始末する）。
    if controller.stop_requested().is_none() {
        runtime.block_on(announce(&server));
    }
    if !controller.set_ready(server.clone(), runtime.handle().clone()) {
        // close 系のハンドラーが起動の終わりを待ちきれずに諦め、プロセスを終わらせ
        // ようとしている（#598 監査 F3・F8）。後始末する者がいないので、終わるまでの
        // 間に `endpoint.json` を消し、収集を止めにいく（間に合うとは限らない。既知の
        // 制限）。ハンドラーが走っていればプロセスの終わりはハンドラーに任せる。
        eprintln!("{SERVER_LABEL}: 起動の途中で閉じられた後に起動が終わったため、すぐに止めます");
        let summary = runtime.block_on(server.shutdown(CLOSE_MARGIN));
        if !controller.claim_exit() {
            loop {
                std::thread::park();
            }
        }
        controller.flush(Duration::from_millis(500));
        std::mem::forget(runtime);
        return summary
            .exit_code()
            .max(server_host::EXIT_SHUTDOWN_INCOMPLETE);
    }

    // 7. 止める合図を待って後始末。
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
    if !controller.claim_exit() {
        // Ctrl-C の後始末の間にウィンドウを閉じた・ログオフ・シャットダウンが来た:
        // その close 系のハンドラーが結果の表示とログの書き出しを終えてから戻り、
        // OS がプロセスを終わらせる。こちらが先に `exit` してハンドラーの最後の行を
        // 切らないよう、ここでは何もせず待つ（#598 の Copilot の指摘）。
        loop {
            std::thread::park();
        }
    }
    // 終わりの行は、終わりを main が受け持つと決まってから出す（#598 監査 F6: close 系の
    // ハンドラーに任せるときに出すと、その `flush` がパイプを閉じた後になりえて、行が
    // ログに入るかが決まらない。そのときはハンドラーが自分の終わりの行を出す）。
    println!("{SERVER_LABEL}: 終了しました（終了コード {code}）");
    controller.flush(Duration::from_secs(2));
    // ランタイムは drop しない（止めたエンジンの残りのタスクを待たない）。A1 の
    // 排他はプロセスの終わりで OS が返す。
    std::mem::forget(runtime);
    code
}

/// 起動の失敗を出して終了コードを返す（ログを開いていればログにも）。
fn report_start_failure(controller: &StopController, failure: &server_host::StartFailure) -> i32 {
    eprintln!("{SERVER_LABEL}: 起動できませんでした: {}", failure.message);
    for line in &failure.guidance {
        eprintln!("{SERVER_LABEL}:   {line}");
    }
    finish_start_failure(controller, failure.exit_code())
}

/// 起動の途中で止める合図を受けた（#598 監査 F3）: 起動を中止して終了コード 3。
fn abort_start(controller: &StopController, origin: StopOrigin) -> i32 {
    eprintln!(
        "{SERVER_LABEL}: 起動の途中で停止の合図（{}）を受けたため、起動を中止します",
        origin.label()
    );
    finish_start_failure(controller, EXIT_START_FAILED)
}

/// 起動の失敗・中止の終わり: 終了の行を出し、**ログを書き出し終えてから**、起動に
/// 失敗したこと（`Phase::Failed`）を知らせる（#598 のオーナーレビュー P2・監査 F1）。
/// 起動中に閉じたウィンドウのハンドラーが起動の終わりを待っていると、`Failed` の
/// 通知で起き、ログを閉じて戻る（プロセスが終わる）ので、先に知らせると理由と対処が
/// 日次ログに残らない。通知はここだけで行い、各分岐で順序を誤らないようにする。
fn finish_start_failure(controller: &StopController, code: i32) -> i32 {
    eprintln!("{SERVER_LABEL}: 終了します（終了コード {code}）");
    controller.flush(Duration::from_secs(2));
    controller.set_phase(Phase::Failed { code });
    code
}

/// デバッグビルドだけ: 環境変数 `CHRONOGAZER_TEST_SLOW_OPEN_MS` があれば、DB を開く
/// 前に、止める合図が来るか、その ms が過ぎるまで待つ（起動の途中の停止の統合テスト
/// 用。待ちに入ったことを 1 行出すので、テストはそれを見てから合図を送る）。
#[cfg(debug_assertions)]
fn test_slow_open(controller: &StopController) {
    let Some(ms) = std::env::var("CHRONOGAZER_TEST_SLOW_OPEN_MS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
    else {
        return;
    };
    eprintln!("{SERVER_LABEL}: テスト用に DB を開く前で待ちます（CHRONOGAZER_TEST_SLOW_OPEN_MS）");
    let deadline = Instant::now() + Duration::from_millis(ms);
    let mut state = controller.lock();
    while state.requested.is_none() {
        let now = Instant::now();
        if now >= deadline {
            return;
        }
        state = controller
            .changed
            .wait_timeout(state, deadline - now)
            .unwrap_or_else(|p| p.into_inner())
            .0;
    }
}

async fn announce(server: &StartedServer) {
    let engine = server.engine();
    if let Some(addr) = engine.loopback_addr().await {
        println!(
            "{SERVER_LABEL}: 待ち受け（この PC から）: http://127.0.0.1:{}/",
            addr.port()
        );
        if !addr.ip().is_loopback() {
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
    if server.exposes_lan().await {
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

/// 開いたログ・向け直しの後始末・パイプを通さない記録先（[`StopController::set_record`]）。
type OpenedLog = (Option<Arc<Mutex<ServerLog>>>, Flush, Option<LineSink>);

/// 日次ログを開き、標準出力・標準エラー出力をそこへ向け直す。
///
/// - ログのファイルを開けない（`logs\` に書けない）ときは、コンソールだけで続ける
///   （`Ok((None, ..))`。書けない場所には向け直しようがない。理由はコンソールに出す）。
/// - ログは開けたが向け直せない（`SetStdHandle` の失敗）ときは `Err`: 起動を止める。
///   理由はログファイルに直接書いておく（向け直さずに動くと、core と依存 crate の
///   `eprintln!` が日次ログに入らず、起動・停止・失敗の診断を失うため）。
#[cfg(windows)]
fn open_log(layout: &DataLayout) -> Result<OpenedLog, String> {
    let log = match ServerLog::open(layout.logs_dir(), Arc::new(banto_tstore::SystemClock)) {
        Ok(log) => Arc::new(Mutex::new(log)),
        Err(err) => {
            eprintln!(
                "{SERVER_LABEL}: ログ（{}）を開けませんでした。コンソールだけに出します: {err}",
                layout.logs_dir().display()
            );
            return Ok((None, Box::new(|_| {}), None));
        }
    };
    match console::redirect_stdio(log.clone()) {
        Ok(redirect) => {
            let redirect = Arc::new(redirect);
            let through = Arc::clone(&redirect);
            Ok((
                Some(log),
                Box::new(move |wait| redirect.finish(wait)),
                Some(Arc::new(move |line: &str| through.write_through(line))),
            ))
        }
        Err(err) => {
            let reason = format!(
                "標準出力・標準エラー出力をログへ向け直せませんでした。日次ログに診断が残らないため、起動しません: {err}"
            );
            if let Ok(mut log) = log.lock() {
                log.write_line(&format!("{SERVER_LABEL}: {reason}"));
                log.flush();
            }
            Err(reason)
        }
    }
}

#[cfg(not(windows))]
fn open_log(_layout: &DataLayout) -> Result<OpenedLog, String> {
    Ok((None, Box::new(|_| {}), None))
}

/// 止める合図の受け口を用意する。Windows のコンソールの Ctrl のハンドラーを
/// 登録できなければ `Err`（#598 の Copilot の指摘: 登録に失敗したまま起動すると、
/// Ctrl-C やコンソールを閉じたときに Windows の既定の終了になり、`endpoint.json` の
/// 削除と最終の書き出しを飛ばす。制御された停止の手段が無いまま収集させない）。
/// デバッグビルドだけ、環境変数 `CHRONOGAZER_TEST_FAIL_CTRL_HANDLER=1` で登録の失敗を
/// 注入できる（統合テスト用。デスクトップの `CHRONOGAZER_TEST_FAIL_TRAY` と同じ扱い）。
fn install_stop_sources(
    controller: &Arc<StopController>,
    options: &CliOptions,
) -> Result<(), String> {
    #[cfg(debug_assertions)]
    if std::env::var("CHRONOGAZER_TEST_FAIL_CTRL_HANDLER").as_deref() == Ok("1") {
        return Err(
            "テスト用に登録の失敗を注入しました（CHRONOGAZER_TEST_FAIL_CTRL_HANDLER）".to_string(),
        );
    }
    #[cfg(windows)]
    {
        let handler = controller.clone();
        let installed = console::install_ctrl_handler(Box::new(move |kind| match kind {
            console::CtrlKind::Interrupt => handler.request(StopOrigin::Interrupt),
            console::CtrlKind::Close => close_and_exit(&handler, StopOrigin::Close),
            console::CtrlKind::Logoff => close_and_exit(&handler, StopOrigin::Logoff),
            console::CtrlKind::Shutdown => close_and_exit(&handler, StopOrigin::Shutdown),
        }));
        installed.map_err(|err| err.to_string())?;
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
                        "close" => close_and_exit(&handler, StopOrigin::Close),
                        _ => {}
                    },
                }
            }
        });
    }
    Ok(())
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

    /// #598 の Copilot の指摘: close 系のハンドラーが走っている間は main が終わりに
    /// 入らない（その終わりを待つ）。main が終わりに入った後の close 系のハンドラーは
    /// 何もしない（main の書き出しを邪魔しない）。
    #[test]
    fn main_yields_to_a_running_close_handler_and_vice_versa() {
        let controller = StopController::new();
        assert!(controller.begin_close());
        assert!(
            !controller.claim_exit(),
            "close のハンドラーの途中で main が終わった"
        );

        let controller = StopController::new();
        assert!(controller.claim_exit());
        assert!(
            !controller.begin_close(),
            "main の終わりの後に close を始めた"
        );
    }

    fn finished_summary() -> ServerShutdownSummary {
        ServerShutdownSummary {
            report: chronogazer_core::engine::ShutdownReport {
                completed: Vec::new(),
                collect_stop_error: None,
                timed_out: false,
                listener_stop_abandoned: false,
                budget: Duration::from_secs(1),
            },
            endpoint_cleared: true,
        }
    }

    /// #598 のレビュー（banto-hub #587 と同じ型）: 後始末の本体の結果の記録は、本体を
    /// 走らせたワーカーの中で結果を公開する前に済む。記録が遅くても、後から来た
    /// 待ち手（`ReceivedPrior`）が戻った時点で完了の行は既にあり、ちょうど 1 行。
    #[test]
    fn the_body_result_is_recorded_before_a_waiter_returns() {
        let once = Arc::new(ExitCleanupOnce::new());
        let lines = Arc::new(Mutex::new(Vec::<String>::new()));
        let slow_record: LineSink = {
            let lines = lines.clone();
            Arc::new(move |line: &str| {
                std::thread::sleep(Duration::from_millis(300));
                lines.lock().unwrap().push(line.to_string());
            })
        };
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let first = {
            let (once, record) = (once.clone(), slow_record.clone());
            std::thread::spawn(move || {
                run_cleanup(
                    &once,
                    StopOrigin::Interrupt,
                    Duration::from_secs(10),
                    move |_| {
                        let _ = entered_tx.send(());
                        std::thread::sleep(Duration::from_millis(100));
                        finished_summary()
                    },
                    record,
                )
            })
        };
        entered_rx.recv().unwrap();
        // 本体の途中で後から来た待ち手（閉じたウィンドウのハンドラー）。
        let report = run_cleanup(
            &once,
            StopOrigin::Close,
            Duration::from_secs(10),
            |_| unreachable!("本体は 1 回だけ"),
            slow_record,
        );
        assert!(
            matches!(report, ExitCleanupReport::ReceivedPrior { .. }),
            "{report:?}"
        );
        assert_eq!(
            *lines.lock().unwrap(),
            vec!["chronogazer-server: 終了の後始末が完了しました（Ctrl-C）".to_string()],
            "待ち手が戻った時点で完了の行が無い・2 行ある"
        );
        assert!(matches!(
            first.join().unwrap(),
            ExitCleanupReport::RanWork(_)
        ));
        assert_eq!(lines.lock().unwrap().len(), 1);
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
        let controller = Arc::new(StopController::new());
        let started = Instant::now();
        assert!(matches!(
            controller.cleanup(StopOrigin::Close, Duration::from_millis(200)),
            Err(NoEngine::StartAbandoned)
        ));
        assert!(started.elapsed() >= Duration::from_millis(200));

        let controller = Arc::new(StopController::new());
        let waiter = {
            let controller = controller.clone();
            std::thread::spawn(move || {
                controller.cleanup(StopOrigin::Close, Duration::from_secs(10))
            })
        };
        controller.set_phase(Phase::Failed { code: 2 });
        assert!(matches!(
            waiter.join().unwrap(),
            Err(NoEngine::StartFailed { code: 2 })
        ));
    }

    /// #598 監査 F8: close 系のハンドラーが起動の終わりを待ちきれずに諦めたら、後から
    /// 起動が終わっても `Ready` にしない（main がすぐ止める）。諦めていなければ `Ready`。
    #[test]
    fn a_start_abandoned_by_the_close_handler_is_not_published_as_ready() {
        let controller = StopController::new();
        assert!(controller
            .cleanup(StopOrigin::Close, Duration::ZERO)
            .is_err_and(|reason| reason == NoEngine::StartAbandoned));
        assert!(controller.lock().start_abandoned);
        // 起動に失敗したことを知らされた待ちは「諦め」ではない。
        let controller = StopController::new();
        controller.set_phase(Phase::Failed { code: 3 });
        assert!(controller
            .cleanup(StopOrigin::Close, Duration::ZERO)
            .is_err_and(|reason| reason == NoEngine::StartFailed { code: 3 }));
        assert!(!controller.lock().start_abandoned);
        assert_eq!(
            controller.report(StopOrigin::Close, Err(NoEngine::StartAbandoned)),
            server_host::EXIT_SHUTDOWN_INCOMPLETE
        );
        assert_eq!(
            controller.report(StopOrigin::Close, Err(NoEngine::StartFailed { code: 2 })),
            2,
            "起動の失敗の終了コード（A1 の 2）を close 系のハンドラーも使う"
        );
    }

    /// #598 監査 F3: 合図は記録した時点で main から見える（起動の段の間で中止する）。
    #[test]
    fn a_stop_request_is_visible_to_the_starting_main() {
        let controller = StopController::new();
        assert_eq!(controller.stop_requested(), None);
        controller.request(StopOrigin::Interrupt);
        assert_eq!(controller.stop_requested(), Some(StopOrigin::Interrupt));
        controller.request(StopOrigin::Close);
        assert_eq!(controller.stop_requested(), Some(StopOrigin::Interrupt));
    }

    /// #598 のオーナーレビュー P2・監査 F1: 起動の失敗は、理由・対処・終了の行を出して
    /// ログを書き出し終えてから `Failed` を知らせる。起動の終わりを待っている close 系の
    /// ハンドラー（`cleanup`）が戻った時点で、書き出しは終わっている。
    #[test]
    fn a_start_failure_is_flushed_before_the_waiting_close_handler_wakes() {
        let controller = Arc::new(StopController::new());
        let flushed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        {
            let flushed = flushed.clone();
            controller.set_flush(Box::new(move |_wait| {
                std::thread::sleep(Duration::from_millis(100));
                flushed.store(true, std::sync::atomic::Ordering::SeqCst);
            }));
        }
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let waiter = {
            let controller = controller.clone();
            let flushed = flushed.clone();
            std::thread::spawn(move || {
                let _ = entered_tx.send(());
                let result = controller.cleanup(StopOrigin::Close, Duration::from_secs(10));
                (result, flushed.load(std::sync::atomic::Ordering::SeqCst))
            })
        };
        entered_rx.recv().unwrap();
        let failure = server_host::StartFailure {
            kind: chronogazer_core::startup_recovery::FailureKind::Other,
            message: "テスト".to_string(),
            guidance: Vec::new(),
        };
        assert_eq!(report_start_failure(&controller, &failure), 3);
        let (result, flushed_when_woken) = waiter.join().unwrap();
        assert!(matches!(result, Err(NoEngine::StartFailed { code: 3 })));
        assert!(
            flushed_when_woken,
            "ログの書き出しの前に close 系のハンドラーが起きた"
        );
    }

    /// 合図は最初のものが残り、待っている main を起こす。
    #[test]
    fn the_first_request_wins_and_wakes_the_waiter() {
        let controller = Arc::new(StopController::new());
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
