//! `chronogazer-server`: 画面の無い本番用の ChronoGazer の記録エンジン（#392 段階 B
//! の B1-5、2026-10-13。設計は docs/chronogazer-service-design.md §3.1(b)）。
//!
//! B1 ではコンソールで動かす形だけ（引数なし）。B3-2b（2026-10-15）で同じ exe に
//! Windows サービス（`run-service`・`install`・`uninstall`、Windows だけ）を足した
//! （`chronogazer_server/service.rs`。下の「Windows サービス」）。開発・E2E 用の
//! `banto-serve` とは別物で、あちらの既定（`0.0.0.0`・`./banto-dev.sqlite3`・
//! `BANTO_HUB_MEMORY_KEYS`）は持ち込まない。
//!
//! ```text
//! chronogazer-server.exe            コンソールで起動（Ctrl-C で停止）
//! chronogazer-server.exe --help     使い方
//! chronogazer-server.exe install [--install-user-sid S-1-5-21-…]
//!                                   サービス `ChronoGazer` を登録（管理者、Windows だけ）
//! chronogazer-server.exe uninstall  登録を解除（データは残す。管理者、Windows だけ）
//! chronogazer-server.exe run-service
//!                                   SCM がサービスの開始で呼ぶ（手で実行すると終了コード 1）
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
//!    エラー出力をそこへ向け直してコンソールにも写す（Windows。`chronogazer_core::stdio_log`、
//!    Ctrl の扱いは `chronogazer_server/console.rs`）。
//!    **リストア予約の適用と DB の初期化より前**（#598 のオーナーレビュー P2: DB を
//!    開けない理由と対処、リストア予約の警告を日次ログに残す）。
//! 5. リストア予約の適用 → DB の初期化（`LockedServer::open`）→ Hub のキーの保存先 →
//!    エンジン（`OpenedServer::start`）。失敗は理由と対処をコンソールとログに出し、
//!    終了コード 3 で終わる。対処は自動では行わない。**理由・対処を出してログを
//!    書き出し終えてから**起動の失敗を知らせる（起動中に閉じたウィンドウのハンドラーが
//!    待っている。#598 監査 F1）。段の間（DB の初期化の前・エンジンの開始の前・開始の
//!    後）で止める合図を見て、来ていれば起動を中止する（終了コード 3。開始の後なら
//!    案内を出さずに後始末へ。#598 監査 F3）。段の途中（DB を開いている最中など）は
//!    止めない。開始の後は、上限の無い処理（ログの古いファイルの削除・待ち受けの
//!    案内）より**先に**動いていることを公開する（close 系のハンドラーが起動の
//!    終わりを待ちきれずに後始末を飛ばさないため。削除はログのロックの外・別の
//!    スレッド。#598 の Copilot の指摘）。
//!
//!    1.・3.〜5.（エンジンの開始まで）の順序は `chronogazer_server/start.rs`
//!    （`start::start_server`）に 1 か所で置き、B3-2 の Windows サービスと共有する
//!    （#392 B3-2a、挙動は不変）。コンソールは host の種類 `server` と、ACL の利用者
//!    としてこのプロセスの利用者を渡し、止める合図・ログの書き出し・失敗の公開は
//!    [`StopController`] が差し込む（`start::StartHooks`）。
//! 6. 止める合図を待ち、後始末（`run\endpoint.json` を消す → エンジンの終了）を
//!    `banto_exit_once::ExitCleanupOnce` で 1 回だけ行う。Ctrl-C・Ctrl-Break は
//!    15 秒、コンソールのウィンドウを閉じた（`CTRL_CLOSE_EVENT`）・ログオフ・
//!    シャットダウンは 4 秒（OS が待つのは約 5 秒）で、ハンドラーのスレッドが
//!    後始末とログの書き出しの完了を待ち、**自分でプロセスを終わらせる**
//!    （[`close_and_exit`]。ハンドラーから戻ったときに OS が終わらせるのを当てに
//!    しない。#598 監査 F5）。
//!
//! 終了コード: 0 正常、1 後始末が予算内に終わらなかった・最終 flush に失敗、
//! 2 同じデータを別のプロセスが使用中、3 その他の起動の失敗、64 引数が違う
//! （`run-service`・`install`・`uninstall` の後の余計な引数、Windows 以外でのこの 3 つ
//! を含む）。
//!
//! # Windows サービス（#392 B3-2b）
//!
//! - `run-service`: 共有ランナー（`banto_service_control::run_service`）に
//!   `ServerProgram` を渡す。起動の順序はコンソールと同じ `start::start_server`
//!   （host の種類は `service`、ACL の利用者は登録時に記録したインストール利用者の SID）。
//!   **コンソールの Ctrl のハンドラーは登録しない**（サービスのプロセスにも
//!   `CTRL_LOGOFF_EVENT` が届き、コンソールのハンドラーはログオフで後始末して終わる
//!   ため、サインアウトのたびにサービスが止まってしまう）。止める合図は SCM の STOP と
//!   OS の PRESHUTDOWN だけ。SCM の外で実行されたら案内の 2 行（製品共通）を出して
//!   終了コード 1（何も作らない）。
//! - `install`・`uninstall`: 共有の手順（`install_service`・`uninstall_service`）と、
//!   インストール利用者の SID の記録（`Parameters\InstallUserSid`）、ルートの用意。
//!   文言と終了コードの判断は `chronogazer_core::server_service`（どの OS でもテスト）。
//!
//! `--control-stdin`（自動テスト用。使い方には出さない）: 標準入力の行で止める
//! （`ctrl-c` = Ctrl-C と同じ経路、`close` = `CTRL_CLOSE_EVENT` のハンドラーと
//! 同じ関数 [`close_and_exit`] を呼ぶ。標準入力が閉じたら Ctrl-C と同じ）。**デバッグビルドだけ**（`#[cfg(debug_assertions)]`。統合テストは
//! debug ビルドで動く）。リリースビルドでは未知の引数として拒否する（終了コード 64）-
//! 標準入力をつないだ親プロセスなどから停止を指示できる入口を本番に残さないため
//! （デスクトップの `CHRONOGAZER_TEST_FAIL_TRAY` と同じ扱い）。
//!
//! `CHRONOGAZER_TEST_PORT`（E2E 用、#392 B2-2。**デバッグビルドだけ**）: DB を開いた
//! 後・待ち受けの形を決める前に、設定 `server.port`（常時の listener のポート）を
//! その値で保存する（[`test_port_from_env`]）。一時ルートの新しい DB でも E2E の固定の
//! ポート（8807）で待ち受けるため。リリースビルドでは読まない（設定 DB だけが
//! ポートを決める、の約束を本番で崩さない）。

#[cfg(windows)]
#[path = "chronogazer_server/console.rs"]
mod console;
#[cfg(windows)]
#[path = "chronogazer_server/log_prune.rs"]
mod log_prune;
#[cfg(windows)]
#[path = "chronogazer_server/service.rs"]
mod service;
#[path = "chronogazer_server/start.rs"]
mod start;

use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use banto_exit_once::{ExitCleanupOnce, ExitCleanupReport};
use chronogazer_core::instance_lock::ChronoGazerHost;
use chronogazer_core::local_grant::LocalGrantStart;
use chronogazer_core::paths::{DataLayout, DirAccessSource};
use chronogazer_core::server_host::{
    self, ServerLog, ServerShutdownSummary, StartedServer, CLOSE_MARGIN, CLOSE_WAIT,
    EXIT_START_FAILED, EXIT_USAGE, INTERRUPT_BUDGET, SERVER_LABEL,
};
use start::{StartHooks, StartSpec, Started};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mode = match parse_args(&args) {
        Ok(Some(mode)) => mode,
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
    let code = match mode {
        Mode::Console(options) => run(options),
        #[cfg(windows)]
        Mode::RunService => service::run_service_dispatcher(),
        #[cfg(windows)]
        Mode::Install { install_user_sid } => service::install(install_user_sid),
        #[cfg(windows)]
        Mode::Uninstall => service::uninstall(),
    };
    std::process::exit(code);
}

#[derive(Debug, Default, PartialEq, Eq)]
struct CliOptions {
    control_stdin: bool,
}

/// 動かし方（#392 B3-2b）。サービスの 3 つは Windows だけ（それ以外の OS では
/// [`parse_args`] が引数の誤りとして断る）。
#[derive(Debug, PartialEq, Eq)]
enum Mode {
    /// コンソール（引数なし、B1-5 のまま）。
    Console(CliOptions),
    /// SCM がサービスの開始で呼ぶ（`run-service`）。
    #[cfg(windows)]
    RunService,
    /// サービスの登録（`install [--install-user-sid SID]`）。SID を省けば実行した
    /// 利用者（D1）。
    #[cfg(windows)]
    Install {
        install_user_sid: Option<banto_safe_root::InstallUserSid>,
    },
    /// 登録の解除（`uninstall`）。
    #[cfg(windows)]
    Uninstall,
}

/// サービスの部分コマンド（最初の引数だけ。コンソールの引数の後には置けない）。
const SERVICE_COMMANDS: [&str; 3] = [
    chronogazer_core::shell_launch::RUN_SERVICE_ARG,
    "install",
    "uninstall",
];

/// 引数の解釈。`Ok(None)` は使い方の表示だけ。
fn parse_args(args: &[String]) -> Result<Option<Mode>, String> {
    if let Some(command) = args
        .first()
        .filter(|first| SERVICE_COMMANDS.contains(&first.as_str()))
    {
        return parse_service_command(command, &args[1..]).map(Some);
    }
    parse_console_args(args).map(|options| options.map(Mode::Console))
}

/// サービスの部分コマンドの解釈（Windows）。余計な・未知の引数は断る。
#[cfg(windows)]
fn parse_service_command(command: &str, rest: &[String]) -> Result<Mode, String> {
    match command {
        "install" => {
            let mut install_user_sid = None;
            let mut rest = rest.iter();
            while let Some(arg) = rest.next() {
                match arg.as_str() {
                    "--install-user-sid" if install_user_sid.is_none() => {
                        let value = rest
                            .next()
                            .ok_or("--install-user-sid には SID（S-1-5-21-…）を続けてください")?;
                        let sid = banto_safe_root::InstallUserSid::parse(value).map_err(|err| {
                            format!("--install-user-sid の値が使えません（{value}）: {err}")
                        })?;
                        install_user_sid = Some(sid);
                    }
                    other => return Err(format!("install の不明な引数です: '{other}'")),
                }
            }
            Ok(Mode::Install { install_user_sid })
        }
        _ => {
            if let Some(extra) = rest.first() {
                return Err(format!("{command} には引数を付けられません: '{extra}'"));
            }
            Ok(if command == "uninstall" {
                Mode::Uninstall
            } else {
                Mode::RunService
            })
        }
    }
}

/// Windows 以外: サービスの部分コマンドは使えない（引数の誤り、終了コード 64）。
#[cfg(not(windows))]
fn parse_service_command(command: &str, _rest: &[String]) -> Result<Mode, String> {
    Err(format!("'{command}' は Windows だけで使えます"))
}

/// コンソールの引数（B1-5 のまま）。
fn parse_console_args(args: &[String]) -> Result<Option<CliOptions>, String> {
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
    #[cfg(windows)]
    {
        eprintln!("Windows サービス（管理者として実行した PowerShell から）:");
        eprintln!("  chronogazer-server.exe install [--install-user-sid S-1-5-21-…]  サービス ChronoGazer を登録");
        eprintln!("  chronogazer-server.exe uninstall                                 登録を解除（データは残す）");
    }
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
/// 1 回だけ）。`run\endpoint.json` を残したこと（#599）、最終の書き出しの失敗と、
/// 完了（予算切れでなければ。endpoint.json を残したときは完了の行にもそう書き、
/// 「完了しました」だけを残さない）。予算切れはエンジンが自分で 1 行出している
/// （`engine::RunningEngine::shutdown`）。
fn body_log_lines(origin: StopOrigin, summary: &ServerShutdownSummary) -> Vec<String> {
    let mut lines = Vec::new();
    // #599: endpoint.json を残したこと（終了コードは変えない。`exit_code` の doc）。
    let endpoint_left = summary.endpoint_diagnostic();
    let left_note = if endpoint_left.is_some() {
        "。接続先のファイルは残りました"
    } else {
        ""
    };
    lines.extend(endpoint_left);
    if let Some(err) = &summary.report.collect_stop_error {
        lines.push(format!(
            "{SERVER_LABEL}: 最終の書き出しに失敗しました: {err}"
        ));
    }
    if !summary.report.timed_out {
        lines.push(format!(
            "{SERVER_LABEL}: 終了の後始末が完了しました（{}{left_note}）",
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
    // 1. ルート（起動の順序は `chronogazer_server/start.rs`。サービスと共有）。
    let layout = match start::resolve_layout() {
        Ok(layout) => layout,
        Err(code) => return code,
    };

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

    // 3.〜6. A1 の排他 → ログ → リストア予約の適用と DB の初期化 → Hub のキーの
    // 保存先 → エンジン（`start::start_server`。段の間の止める合図と失敗の報告の
    // 順序もそこ）。コンソールは host の種類 `server`、ACL の利用者はこのプロセスの
    // 利用者。
    let Started { server, log } = match start::start_server(
        &runtime,
        StartSpec {
            layout: &layout,
            host: ChronoGazerHost::Server,
            access: &DirAccessSource::CurrentProcess,
        },
        controller.as_ref(),
    ) {
        Ok(started) => started,
        Err(code) => return code,
    };
    // 収集と待ち受けを始めた後は、上限の無い処理（ログの古いファイルの削除、
    // ログへ流れる案内の出力）より**先に**動いていることを公開する（`set_ready`。
    // #598 の Copilot の指摘）。公開の前に長引くと、起動の終わりを待つ close 系の
    // ハンドラーが 4 秒の予算で諦め（`StartAbandoned`）、`server.shutdown` を呼ばずに
    // 最終の書き出しと `collection_stopped` を飛ばす。ここで取るログのロックは
    // 保持日数を書き込むだけ（ファイルシステムに触れない）。
    let log_prune = log.as_ref().and_then(|log| {
        log.lock()
            .ok()
            .and_then(|mut log| log.set_retention(server.log_retention_days()))
    });
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
    // 古いログの削除はログのロックの外・別のスレッドで（終了の最後の行の記録と
    // 書き出しを待たせない。終了はこの削除を待たない）。
    if let Some(prune) = log_prune {
        spawn_log_prune(prune);
    }
    // 起動の最中に合図が来ていたら、待ち受けの案内を出さない（Ctrl-C なら下の
    // `wait_request` がすぐ返り、close 系なら待っていたハンドラーが既に後始末して
    // いる）。案内は公開の後なので、出力が詰まっても終了を待たせない。
    if controller.stop_requested().is_none() {
        runtime.block_on(announce(
            &server,
            &format!("{SERVER_LABEL}: Ctrl-C で停止します"),
        ));
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

/// コンソールの起動の順序の差し込み（`start::start_server`）。
impl StartHooks for StopController {
    fn stop_requested(&self) -> Option<&'static str> {
        StopController::stop_requested(self).map(StopOrigin::label)
    }

    fn open_log(&self, layout: &DataLayout) -> Result<OpenedLog, String> {
        open_log(layout)
    }

    fn log_opened(&self, flush: Flush, record: Option<LineSink>) {
        self.set_flush(flush);
        if let Some(record) = record {
            self.set_record(record);
        }
    }

    fn finish_start_failure(&self, code: i32) -> i32 {
        finish_start_failure(self, code)
    }

    fn before_open(&self) {
        #[cfg(debug_assertions)]
        test_slow_open(self);
    }
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

/// テスト用のポート（`CHRONOGAZER_TEST_PORT`、モジュール doc）を読む。**デバッグ
/// ビルドだけ**値を返し、リリースビルドでは常に `Ok(None)`（読まない）。`raw` は環境
/// 変数の値。1〜65535 の整数でなければ `Err`（起動を止める - 黙って既定のポートで
/// 動くと、E2E が別のサーバーに繋がりうる）。
#[cfg(debug_assertions)]
fn test_port_from_env(raw: Option<&str>) -> Result<Option<u16>, String> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    match raw.trim().parse::<u16>() {
        Ok(port) if port != 0 => Ok(Some(port)),
        _ => Err(format!(
            "CHRONOGAZER_TEST_PORT は 1〜65535 の整数にしてください（{raw}）"
        )),
    }
}

/// リリースビルドの形（読まない）。呼ぶのはテストだけ（`apply_test_port` はデバッグ
/// ビルドだけにある）。
#[cfg(all(test, not(debug_assertions)))]
fn test_port_from_env(_raw: Option<&str>) -> Result<Option<u16>, String> {
    Ok(None)
}

/// デバッグビルドだけ: `CHRONOGAZER_TEST_PORT` があれば設定 `server.port` に保存する
/// （待ち受けの形は `OpenedServer::start` が保存値から決める）。
#[cfg(debug_assertions)]
async fn apply_test_port(opened: &server_host::OpenedServer) -> Result<(), String> {
    let raw = std::env::var("CHRONOGAZER_TEST_PORT").ok();
    let Some(port) = test_port_from_env(raw.as_deref())? else {
        return Ok(());
    };
    let settings = opened.settings();
    let mut config = settings
        .server_config()
        .await
        .map_err(|err| format!("テスト用のポートを保存する前に設定を読めませんでした: {err}"))?;
    config.port = port;
    settings
        .set_server_config(&config)
        .await
        .map_err(|err| format!("テスト用のポートを保存できませんでした: {err}"))?;
    eprintln!(
        "{SERVER_LABEL}: テスト用に server.port を {port} にしました（CHRONOGAZER_TEST_PORT）"
    );
    Ok(())
}

/// 古いログファイルの削除を別のスレッドで走らせる（ログのロックを手放した後に
/// 呼ぶ。`server_host` のモジュール doc「ログ」）。コンソールの経路で、終わりは待たない
/// （プロセスの終わりで打ち切られてよい）。サービスの中の削除は [`run_log_prune`] で
/// 追跡する口（[`log_prune::LogPrunes`]）を通す。
fn spawn_log_prune(prune: server_host::LogPrune) {
    let job = log_prune_job(prune);
    let _ = std::thread::Builder::new()
        .name("chronogazer-log-prune".to_string())
        .spawn(job);
}

/// 削除を走らせる: 追跡する口（サービス）があればそれで、無ければ（コンソール）
/// [`spawn_log_prune`] で切り離して。
#[cfg(windows)]
fn run_log_prune(prunes: Option<&Arc<log_prune::LogPrunes>>, prune: server_host::LogPrune) {
    match prunes {
        Some(prunes) => {
            prunes.spawn(log_prune_job(prune));
        }
        None => spawn_log_prune(prune),
    }
}

/// 削除の 1 回分の仕事。デバッグビルドだけ、環境変数 `CHRONOGAZER_TEST_SLOW_LOG_PRUNE_MS`
/// があれば、削除の前にその ms だけ待つ（起動後の削除が長引いている最中の終了の統合
/// テスト用。待ちに入ったことを 1 行出すので、テストはそれを見てから合図を送る）。
fn log_prune_job(prune: server_host::LogPrune) -> Box<dyn FnOnce() + Send> {
    #[cfg(debug_assertions)]
    if let Some(ms) = std::env::var("CHRONOGAZER_TEST_SLOW_LOG_PRUNE_MS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
    {
        return Box::new(move || {
            eprintln!(
                "{SERVER_LABEL}: テスト用にログの古いファイルの削除の前で待ちます（CHRONOGAZER_TEST_SLOW_LOG_PRUNE_MS）"
            );
            std::thread::sleep(Duration::from_millis(ms));
            prune.run();
        });
    }
    Box::new(move || prune.run())
}

/// 待ち受けの案内。`stop_hint` は最後の 1 行（止め方。コンソールは Ctrl-C、
/// サービスは `Stop-Service`）。
async fn announce(server: &StartedServer, stop_hint: &str) {
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
    // 最初の管理者の作成（`/api/auth/setup`）とこの PC からの接続（`localDesktop`、
    // #392 B2-1）は、常時の listener への、この PC からの要求（peer と Host の両方が
    // loopback）だけで開く（2026-10-14 オーナー決定。`0.0.0.0` にまとめても同じ）。
    if let Some(addr) = engine.loopback_addr().await {
        let port = addr.port();
        if server.exposes_lan().await {
            println!(
                "{SERVER_LABEL}: 最初の管理者の作成とログインなしの接続は、この PC の http://127.0.0.1:{port}/ からだけ行えます（LAN からは閉じています）"
            );
        }
        let local_grant = engine.services().local_grant;
        match local_grant.issued_role() {
            Some(_) if local_grant == LocalGrantStart::Uninitialized => println!(
                "{SERVER_LABEL}: 最初の管理者を作るまでの間、この PC（http://127.0.0.1:{port}/）からはログインなしで管理者として入れます"
            ),
            Some(role) => println!(
                "{SERVER_LABEL}: この PC（http://127.0.0.1:{port}/）からはログインなしで {} として入れます（設定 auth.local_grant）",
                role.as_str()
            ),
            None => {}
        }
    }
    if let Some(err) = server.endpoint_error() {
        eprintln!("{SERVER_LABEL}: {err}");
    }
    println!("{stop_hint}");
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
    redirect_opened_log(log, None)
}

/// サービス（#623 PR-3）: 握った `logs\` に日次ログを**辿らずに**開き（`ServerLog::open_in`）、
/// 向け直す。**開けなければ `Err`**（サービスにはコンソールが無く、起動・収集・停止の診断が
/// 黙って失われるため、起動しない。PR #618 の Codex の指摘）。
///
/// 日付の切り替えで起きる古いログの削除は `prunes`（サービスの追跡する口）を通す
/// （STOP は緩める前にその全部の終わりを待つ。[`log_prune`]）。
#[cfg(windows)]
fn open_log_in(
    dir: Arc<banto_safe_root::ManagedDir>,
    prunes: Arc<log_prune::LogPrunes>,
) -> Result<OpenedLog, String> {
    open_log_in_with_clock(dir, prunes, Arc::new(banto_tstore::SystemClock))
}

/// [`open_log_in`] の時計を渡せる形（テストは `ManualClock` で日付の切り替えを起こす）。
#[cfg(windows)]
fn open_log_in_with_clock(
    dir: Arc<banto_safe_root::ManagedDir>,
    prunes: Arc<log_prune::LogPrunes>,
    clock: Arc<dyn banto_tstore::Clock>,
) -> Result<OpenedLog, String> {
    let path = dir.final_path().to_path_buf();
    let log = ServerLog::open_in(dir, clock).map_err(|err| {
        format!(
            "日次ログ（{}）を開けませんでした。Windows サービスにはコンソールが無く、起動・収集・停止の診断が残らないため、起動しません: {err}",
            path.display()
        )
    })?;
    redirect_opened_log(Arc::new(Mutex::new(log)), Some(prunes))
}

/// 開いた日次ログへ標準出力・標準エラー出力を向け直す（[`open_log`]・[`open_log_in`] で同じ）。
#[cfg(windows)]
fn redirect_opened_log(
    log: Arc<Mutex<ServerLog>>,
    prunes: Option<Arc<log_prune::LogPrunes>>,
) -> Result<OpenedLog, String> {
    match console::redirect_stdio(log.clone(), prunes) {
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
                // 保持日数をまだ読んでいないので削除は返らない。
                let _ = log.write_line(&format!("{SERVER_LABEL}: {reason}"));
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
        assert_eq!(
            parse_args(&[]),
            Ok(Some(Mode::Console(CliOptions::default())))
        );
        assert_eq!(parse_args(&args(&["--help"])), Ok(None));
        assert_eq!(parse_args(&args(&["-h"])), Ok(None));
        // テスト用の隠し引数はデバッグビルドだけ受け、リリースビルドでは拒否する。
        #[cfg(debug_assertions)]
        assert_eq!(
            parse_args(&args(&["--control-stdin"])),
            Ok(Some(Mode::Console(CliOptions {
                control_stdin: true
            })))
        );
        #[cfg(not(debug_assertions))]
        assert!(parse_args(&args(&["--control-stdin"])).is_err());
        for unknown in ["--port", "x", "Install", "--install-user-sid"] {
            assert!(parse_args(&args(&[unknown])).is_err(), "{unknown}");
        }
        // サービスの部分コマンドは最初の引数だけ（コンソールの引数の後には置けない）。
        assert!(parse_args(&args(&["--help", "install"])).is_ok());
        assert!(parse_args(&args(&["x", "run-service"])).is_err());
    }

    /// #392 B3-2b: サービスの部分コマンド（Windows）。余計な・未知の引数は断る
    /// （終了コード 64）。
    #[cfg(windows)]
    #[test]
    fn parse_args_accepts_the_service_commands_on_windows() {
        const SID: &str = "S-1-5-21-1111111111-2222222222-3333333333-1001";
        assert_eq!(
            parse_args(&args(&["run-service"])),
            Ok(Some(Mode::RunService))
        );
        assert_eq!(parse_args(&args(&["uninstall"])), Ok(Some(Mode::Uninstall)));
        assert_eq!(
            parse_args(&args(&["install"])),
            Ok(Some(Mode::Install {
                install_user_sid: None
            }))
        );
        assert_eq!(
            parse_args(&args(&["install", "--install-user-sid", SID])),
            Ok(Some(Mode::Install {
                install_user_sid: Some(banto_safe_root::InstallUserSid::parse(SID).unwrap())
            }))
        );
        for bad in [
            &["run-service", "x"][..],
            &["run-service", "--control-stdin"],
            &["uninstall", "--force"],
            &["install", "x"],
            &["install", "--install-user-sid"],
            // 利用者のアカウントの SID ではない（Administrators・Everyone）。
            &["install", "--install-user-sid", "S-1-5-32-544"],
            &["install", "--install-user-sid", "S-1-1-0"],
            &[
                "install",
                "--install-user-sid",
                SID,
                "--install-user-sid",
                SID,
            ],
        ] {
            assert!(parse_args(&args(bad)).is_err(), "{bad:?}");
        }
    }

    /// #392 B3-2b: Windows 以外ではサービスの部分コマンドを断る（終了コード 64）。
    #[cfg(not(windows))]
    #[test]
    fn parse_args_rejects_the_service_commands_off_windows() {
        for command in ["run-service", "install", "uninstall"] {
            let err = parse_args(&args(&[command])).unwrap_err();
            assert!(err.contains("Windows だけ"), "{err}");
        }
    }

    /// #392 B2-2: `CHRONOGAZER_TEST_PORT` はデバッグビルドだけ読み、リリースビルドでは
    /// どんな値でも無視する（`--control-stdin` と同じ扱い）。
    #[test]
    fn test_port_is_read_only_in_debug_builds() {
        assert_eq!(test_port_from_env(None), Ok(None));
        #[cfg(debug_assertions)]
        {
            assert_eq!(test_port_from_env(Some("8807")), Ok(Some(8807)));
            assert_eq!(test_port_from_env(Some(" 8807 ")), Ok(Some(8807)));
            for bad in ["0", "65536", "-1", "x", ""] {
                assert!(test_port_from_env(Some(bad)).is_err(), "{bad}");
            }
        }
        #[cfg(not(debug_assertions))]
        for any in ["8807", "0", "x"] {
            assert_eq!(test_port_from_env(Some(any)), Ok(None), "{any}");
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
            endpoint: chronogazer_core::endpoint::EndpointClear::Removed,
            endpoint_file: std::path::PathBuf::from("endpoint.json"),
        }
    }

    /// #599: endpoint.json を消せなかったら、本体の記録（結果の公開の前に 1 回）に
    /// その 1 行が入り、完了の行も「残った」と書く（「完了しました」だけにしない）。
    /// 終了コードは変えない（`ServerShutdownSummary::exit_code` の doc）。消せたときは
    /// 何も足さない。
    #[test]
    fn a_left_endpoint_is_recorded_once_without_changing_the_exit_code() {
        let once = ExitCleanupOnce::new();
        let lines = Arc::new(Mutex::new(Vec::<String>::new()));
        let record: LineSink = {
            let lines = lines.clone();
            Arc::new(move |line: &str| lines.lock().unwrap().push(line.to_string()))
        };
        let report = run_cleanup(
            &once,
            StopOrigin::Close,
            Duration::from_secs(10),
            |_| ServerShutdownSummary {
                endpoint: chronogazer_core::endpoint::EndpointClear::RemoveFailed(
                    "access denied".to_string(),
                ),
                ..finished_summary()
            },
            record,
        );
        let ExitCleanupReport::RanWork(banto_exit_once::ExitCleanupOutcome::Finished(summary)) =
            report
        else {
            panic!("本体を走らせたはず: {report:?}");
        };
        assert_eq!(summary.exit_code(), server_host::EXIT_OK);
        let lines = lines.lock().unwrap().clone();
        assert_eq!(lines.len(), 2, "{lines:?}");
        assert!(
            lines[0].contains("消せませんでした") && lines[0].contains("access denied"),
            "{lines:?}"
        );
        assert!(
            lines[1].contains("完了しました") && lines[1].contains("残りました"),
            "{lines:?}"
        );

        let quiet = body_log_lines(StopOrigin::Interrupt, &finished_summary());
        assert_eq!(quiet.len(), 1, "{quiet:?}");
        assert!(!quiet[0].contains("残りました"), "{quiet:?}");
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
        assert_eq!(
            start::report_start_failure(controller.as_ref(), &failure),
            3
        );
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
