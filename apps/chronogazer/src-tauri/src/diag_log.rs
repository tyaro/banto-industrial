//! デスクトップの診断のログ（2026-10-24 オーナー決定「案 A」）。
//!
//! リリースビルドのデスクトップは `windows_subsystem = "windows"`（`main.rs`）で標準エラー
//! 出力の行き先が無く、デスクトップで収集している間の core と依存 crate の `eprintln!`（書き込みの
//! 失敗・セッションの起動の失敗など）が消えていた。そこで `chronogazer-server` と同じ仕組み
//! （`chronogazer_core::stdio_log`: 標準ハンドルを匿名パイプへ差し替え、読み取り側のスレッドが
//! 日次ファイルへ書く）で、`logs\chronogazer-desktop-YYYY-MM-DD.log`
//! （`chronogazer_core::server_host::ServerLog::open_desktop`）へ向け直す。デバッグビルド
//! （コンソールがある）では、サーバーと同じくコンソールにも写す（tee）。
//!
//! # いつ書くか（[`DiagnosticLog`]）
//!
//! `logs\` に書いてよいのは**設定モードの間だけ**（運用モードの間は利用者に読み取りだけ。
//! 2026-10-18 オーナー決定 (3)）。そこで向け直すのは**この プロセスが A1 の排他を持っている
//! 間だけ**にする:
//!
//! - **始める**（[`DiagnosticLog::start`]）: `crate::launch_desktop` で `EngineRuntime::open` が
//!   成功した直後（A1 の排他を取った後）。サービスは A1 を取ってから ACL を締める
//!   （`chronogazer-server` の `service.rs` の起動の 5 → 8・9）ので、この プロセスが A1 を
//!   持つ間に `logs\` が締まることは無い。Desktop の画面になるのは、ルートが運用モードの形で
//!   ないときだけ（`shell_launch::decide` の `RootBlocked`）。
//! - **外す**（[`DiagnosticLog::stop`]）: 起動に失敗したとき（`crate::launch_desktop` が `Err`
//!   を返す前。エンジンは A1 を返すので、同じプロセスのやり直しでまた始める）。
//! - **封じる**（[`DiagnosticLog::seal`]）: 終了の後始末（`crate::exit_cleanup`）でエンジンを
//!   止めた後。トレイの「終了」・OS の終了（`RunEvent::Exit`）・**運用モードへの切り替え**
//!   （`switch::hand_off` は `exit_cleanup` → 新しいシェルを起動 → 終わる）が通る。運用モードへの
//!   切り替えでは、新しいシェルはこのプロセスが終わるのを待ってからサービスを開始する（ACL を
//!   締めるのはサービス）ので、締める前に必ず外れている。封じた後は始めない（終了と起動の
//!   やり直しが重なっても、終わる間際に向け直し直さない）。
//!
//! 外す・封じるの順はサーバーの `close_log` と同じ: 向け直しの残りを流しきる
//! （`StdioRedirect::finish`。標準ハンドルを元に戻し、番兵より前の行をファイルへ書き出す）→
//! 日次ログを閉じる（`ServerLog::close`。番兵の後に遅れて来た行も書かない）。**判断**: サービス
//! と違い、外した後に `NUL` へは向けない - 元の標準エラー出力はデバッグビルドならコンソール、
//! リリースビルドなら無い（書いても捨てられる）ので、元に戻せば足りる。
//!
//! # 失敗の扱い
//!
//! 向け直しに失敗しても（`logs\` を作れない・開けない・パイプを作れない）、起動・収集は止めない
//! （ログが書けないことで止めない、既存の方針）。理由は元の標準エラー出力へ出し、開けたログには
//! 1 行残す。
//!
//! # 同じファイルに書くもの・古いファイル
//!
//! **判断**: 診断の行は、モードの切り替えの行（`mode_switch::append_desktop_log`）と**同じ
//! ファイル**に混ぜる。どちらも行の形は `[YYYY-MM-DD HH:MM:SS] ...`（ローカル時刻、
//! `server_host::format_timestamp`）で揃っている。切り替えの行は起きた時刻で書くので、前後の
//! 診断の行より時刻が前になることがある（書く時刻ではなく起きた時刻、の約束のまま）。どちらも
//! 追記で開くので、1 回の書き込みの単位では交ざらない。
//!
//! 古いファイルの削除は、エンジンが動き始めてから設定の保持日数（`log.retention_days`、
//! `server_host::parse_log_retention`。読めなければ削除しない）を
//! [`DiagnosticLog::set_retention`] で渡し、`chronogazer-server` のコンソール版と同じく切り離した
//! スレッドで消す（`server_host::log_files_to_prune` は `chronogazer-server-`・
//! `chronogazer-desktop-` の両方の形を受ける - サーバーの保持と同じ）。日付をまたいで動けば
//! 日付の切り替えでも消す。デスクトップは利用者の権限で、設定モードの間（A1 を持つ間）だけ消す。

use std::path::Path;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

/// 外す・封じるときに、向け直しの残りを流しきるのを待つ上限（ふつうは数ミリ秒で終わる）。
pub(crate) const FLUSH_WAIT: Duration = Duration::from_secs(1);

/// 向け直している間の口（本番は [`production_starter`]。テストは偽物で順を数える）。
pub(crate) trait ActiveRedirect: Send {
    /// 保持日数を渡す（`None` = 消さない）。古いものの削除はログのロックの外で走らせる。
    fn set_retention(&self, retention_days: Option<u32>);
    /// 流しきる（標準ハンドルを元に戻し、残りを書き出す）→ ログを閉じる。
    fn close(self: Box<Self>, wait: Duration);
}

/// `logs\` を受けて向け直しを始める口。
pub(crate) type Starter =
    Box<dyn Fn(&Path) -> std::io::Result<Box<dyn ActiveRedirect>> + Send + Sync>;

#[derive(Default)]
struct State {
    active: Option<Box<dyn ActiveRedirect>>,
    /// [`DiagnosticLog::seal`] の後か（以後は始めない）。
    sealed: bool,
}

/// デスクトップの診断のログの状態（モジュール doc「いつ書くか」）。標準ハンドルはプロセスに
/// 1 つなので、本番はプロセスで 1 つ（[`global`]）。操作は 1 つのロックの中で行う（始める・
/// 外すが重ならない）。
pub(crate) struct DiagnosticLog {
    starter: Starter,
    state: Mutex<State>,
}

/// [`DiagnosticLog::start`] の結果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StartOutcome {
    /// 向け直した。
    Started,
    /// 既に向け直している（何もしない）。
    AlreadyActive,
    /// 封じた後なので始めない。
    Sealed,
    /// 向け直せなかった（起動・収集は続ける）。
    Failed,
}

impl DiagnosticLog {
    pub(crate) fn new(starter: Starter) -> Self {
        Self {
            starter,
            state: Mutex::new(State::default()),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// `logs_dir` の日次ファイルへ向け直す（A1 の排他を持った後に呼ぶ）。失敗は元の標準エラー
    /// 出力へ 1 行出して `Failed`（止めない）。
    pub(crate) fn start(&self, logs_dir: &Path) -> StartOutcome {
        let mut state = self.lock();
        if state.sealed {
            return StartOutcome::Sealed;
        }
        if state.active.is_some() {
            return StartOutcome::AlreadyActive;
        }
        match (self.starter)(logs_dir) {
            Ok(active) => {
                state.active = Some(active);
                StartOutcome::Started
            }
            Err(err) => {
                eprintln!(
                    "banto: 診断のログ（{}）へ標準出力・標準エラー出力を向け直せませんでした。記録はこのまま続けます: {err}",
                    logs_dir.display()
                );
                StartOutcome::Failed
            }
        }
    }

    /// 保持日数を渡す（向け直していなければ何もしない）。
    pub(crate) fn set_retention(&self, retention_days: Option<u32>) {
        if let Some(active) = self.lock().active.as_ref() {
            active.set_retention(retention_days);
        }
    }

    /// 向け直しを外す（流しきる → 閉じる）。後でまた始められる（起動のやり直し）。
    pub(crate) fn stop(&self, wait: Duration) {
        let mut state = self.lock();
        if let Some(active) = state.active.take() {
            active.close(wait);
        }
    }

    /// 向け直しを外し、以後は始めない（終了・運用モードへの切り替え）。何度呼んでもよい。
    pub(crate) fn seal(&self, wait: Duration) {
        let mut state = self.lock();
        state.sealed = true;
        if let Some(active) = state.active.take() {
            active.close(wait);
        }
    }

    /// 向け直しているか。
    #[cfg(test)]
    pub(crate) fn is_active(&self) -> bool {
        self.lock().active.is_some()
    }
}

/// `body`（終了の後始末の本体: エンジンを止める）を走らせ、その**後で**封じる（エンジンの
/// 停止の診断と後始末の結果の行まで日次ログに残してから外す）。`crate::exit_cleanup` が使う。
pub(crate) fn run_then_seal<R>(log: &DiagnosticLog, wait: Duration, body: impl FnOnce() -> R) -> R {
    let result = body();
    log.seal(wait);
    result
}

/// プロセスで 1 つの診断のログ（本番の向け直し）。
pub(crate) fn global() -> &'static DiagnosticLog {
    static GLOBAL: OnceLock<DiagnosticLog> = OnceLock::new();
    GLOBAL.get_or_init(|| DiagnosticLog::new(production_starter()))
}

/// 本番の向け直し（Windows: `chronogazer_core::stdio_log`）。
#[cfg(windows)]
fn production_starter() -> Starter {
    Box::new(windows_redirect::start)
}

/// Windows 以外は向け直さない（何もしない口）。
#[cfg(not(windows))]
fn production_starter() -> Starter {
    struct Inert;
    impl ActiveRedirect for Inert {
        fn set_retention(&self, _retention_days: Option<u32>) {}
        fn close(self: Box<Self>, _wait: Duration) {}
    }
    Box::new(|_logs_dir| Ok(Box::new(Inert) as Box<dyn ActiveRedirect>))
}

#[cfg(windows)]
mod windows_redirect {
    use std::path::Path;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use chronogazer_core::server_host::ServerLog;
    use chronogazer_core::stdio_log::{redirect_stdio, PruneRunner, StdioRedirect};

    use super::ActiveRedirect;

    /// 向け直している間（日次ログと向け直しの口）。
    struct Redirected {
        log: Arc<Mutex<ServerLog>>,
        redirect: StdioRedirect,
    }

    /// `logs_dir` の今日の `chronogazer-desktop-` のファイルを開き、向け直す。向け直せなければ
    /// 理由をそのファイルに 1 行残して閉じ、`Err`。
    pub(super) fn start(logs_dir: &Path) -> std::io::Result<Box<dyn ActiveRedirect>> {
        let log = Arc::new(Mutex::new(ServerLog::open_desktop_now(logs_dir)?));
        // 日付の切り替えで起きる削除は切り離して走らせる（コンソール版と同じ。プロセスの
        // 終わりで打ち切られてよい - 今書いているファイルには触れない）。
        let run_prune: PruneRunner = Arc::new(|prune| prune.spawn());
        match redirect_stdio(Arc::clone(&log), run_prune) {
            Ok(redirect) => Ok(Box::new(Redirected { log, redirect })),
            Err(err) => {
                if let Ok(mut log) = log.lock() {
                    // 保持日数をまだ渡していないので削除は返らない。
                    let _ = log.write_line(&format!(
                        "banto: 標準出力・標準エラー出力をこのログへ向け直せませんでした（診断は残りません。記録は続けます）: {err}"
                    ));
                    log.close();
                }
                Err(err)
            }
        }
    }

    impl ActiveRedirect for Redirected {
        fn set_retention(&self, retention_days: Option<u32>) {
            let prune = self
                .log
                .lock()
                .ok()
                .and_then(|mut log| log.set_retention(retention_days));
            if let Some(prune) = prune {
                prune.spawn();
            }
        }

        fn close(self: Box<Self>, wait: Duration) {
            // サーバーの `close_log` と同じ順: 流しきる（標準ハンドルを戻し、番兵より前の行を
            // 書き出す）→ ログを閉じる（番兵の後に遅れて来た行も書かない）。
            self.redirect.finish(wait);
            if let Ok(mut log) = self.log.lock() {
                log.close();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    type Trace = Arc<Mutex<Vec<String>>>;

    struct Fake {
        trace: Trace,
    }

    impl ActiveRedirect for Fake {
        fn set_retention(&self, retention_days: Option<u32>) {
            self.trace
                .lock()
                .unwrap()
                .push(format!("retention {retention_days:?}"));
        }

        fn close(self: Box<Self>, _wait: Duration) {
            let mut trace = self.trace.lock().unwrap();
            trace.push("flush".to_string());
            trace.push("close".to_string());
        }
    }

    fn fake_log(fail: bool) -> (DiagnosticLog, Trace) {
        let trace: Trace = Arc::default();
        let sink = Arc::clone(&trace);
        let log = DiagnosticLog::new(Box::new(move |dir: &Path| {
            sink.lock()
                .unwrap()
                .push(format!("start {}", dir.display()));
            if fail {
                return Err(std::io::Error::other("注入した失敗"));
            }
            Ok(Box::new(Fake {
                trace: Arc::clone(&sink),
            }) as Box<dyn ActiveRedirect>)
        }));
        (log, trace)
    }

    fn take(trace: &Trace) -> Vec<String> {
        std::mem::take(&mut *trace.lock().unwrap())
    }

    /// 始める → 保持日数 → 外す（流しきる → 閉じる）の順。外した後は保持日数を渡さない。
    /// 起動のやり直しのため、外した後はまた始められる。
    #[test]
    fn start_retention_and_stop_in_order_and_restart_after_stop() {
        let (log, trace) = fake_log(false);
        assert_eq!(log.start(Path::new("logs")), StartOutcome::Started);
        assert_eq!(log.start(Path::new("logs")), StartOutcome::AlreadyActive);
        log.set_retention(Some(30));
        log.stop(FLUSH_WAIT);
        assert!(!log.is_active());
        log.set_retention(Some(7));
        log.stop(FLUSH_WAIT);
        assert_eq!(
            take(&trace),
            ["start logs", "retention Some(30)", "flush", "close"]
        );
        assert_eq!(log.start(Path::new("logs")), StartOutcome::Started);
        assert!(log.is_active());
    }

    /// 封じた後は始めない（終了・運用モードへの切り替えの後に向け直し直さない）。封じるのは
    /// 何度でもよく、外すのは 1 回だけ。
    #[test]
    fn seal_closes_once_and_refuses_later_starts() {
        let (log, trace) = fake_log(false);
        assert_eq!(log.start(Path::new("logs")), StartOutcome::Started);
        log.seal(FLUSH_WAIT);
        log.seal(FLUSH_WAIT);
        assert_eq!(log.start(Path::new("logs")), StartOutcome::Sealed);
        assert_eq!(take(&trace), ["start logs", "flush", "close"]);
    }

    /// 向け直せなくても止めない（`Failed` を返すだけ）。後でまた試せる。
    #[test]
    fn a_failed_start_is_reported_and_does_not_stick() {
        let (log, trace) = fake_log(true);
        assert_eq!(log.start(Path::new("logs")), StartOutcome::Failed);
        assert!(!log.is_active());
        log.stop(FLUSH_WAIT);
        assert_eq!(log.start(Path::new("logs")), StartOutcome::Failed);
        assert_eq!(take(&trace), ["start logs", "start logs"]);
    }

    /// 終了の後始末: 本体（エンジンの停止）が終わってから流しきって閉じる（本体の診断の行が
    /// ログに残る）。
    #[test]
    fn exit_cleanup_seals_after_the_body() {
        let (log, trace) = fake_log(false);
        assert_eq!(log.start(Path::new("logs")), StartOutcome::Started);
        let body_trace = Arc::clone(&trace);
        let result = run_then_seal(&log, FLUSH_WAIT, || {
            body_trace
                .lock()
                .unwrap()
                .push("engine stopped".to_string());
            7
        });
        assert_eq!(result, 7);
        assert_eq!(
            take(&trace),
            ["start logs", "engine stopped", "flush", "close"]
        );
        assert_eq!(log.start(Path::new("logs")), StartOutcome::Sealed);
    }

    /// 本物の向け直し（Windows）: 向け直している間の `eprintln!` が
    /// `chronogazer-desktop-YYYY-MM-DD.log` に `[時刻] 行` の形で残り、外した後の行は残らない。
    /// 保持日数を渡すと古い日付のファイル（デスクトップ・サーバーの両方の形）を消す。
    /// 標準ハンドルはプロセスに 1 つなので、このテストだけが本物を向け直す。
    #[cfg(windows)]
    #[test]
    fn the_real_redirect_writes_eprintln_to_the_desktop_log_until_sealed() {
        let dir = tempfile::tempdir().unwrap();
        let logs = dir.path().join("logs");
        std::fs::create_dir_all(&logs).unwrap();
        let old_desktop = logs.join("chronogazer-desktop-2000-01-01.log");
        let old_server = logs.join("chronogazer-server-2000-01-01.log");
        std::fs::write(&old_desktop, "old\n").unwrap();
        std::fs::write(&old_server, "old\n").unwrap();

        // テストの出力の捕捉（`eprintln!`）を通らないよう、標準エラー出力へ直接書く
        // （本番の `eprintln!` と同じく、書くたびに `GetStdHandle` を読む）。
        use std::io::Write as _;
        let log = DiagnosticLog::new(production_starter());
        assert_eq!(log.start(&logs), StartOutcome::Started);
        let _ = writeln!(std::io::stderr(), "diag-log-test: 向け直している間の行");
        log.set_retention(Some(30));
        log.seal(Duration::from_secs(5));
        let _ = writeln!(std::io::stderr(), "diag-log-test: 外した後の行");

        let today: Vec<_> = std::fs::read_dir(&logs)
            .unwrap()
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| {
                        name.starts_with("chronogazer-desktop-") && !name.contains("2000-01-01")
                    })
            })
            .collect();
        assert_eq!(today.len(), 1, "{today:?}");
        let path = today[0].clone();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.lines().any(|line| line.starts_with('[')
                && line.ends_with("] diag-log-test: 向け直している間の行")),
            "{text}"
        );
        assert!(!text.contains("外した後の行"), "{text}");
        // 削除は切り離したスレッドで走る。
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while (old_desktop.exists() || old_server.exists()) && std::time::Instant::now() < deadline
        {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(!old_desktop.exists() && !old_server.exists());
        assert!(path.exists(), "今日のファイルは消さない");
    }
}
