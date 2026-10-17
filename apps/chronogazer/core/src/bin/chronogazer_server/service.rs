//! `chronogazer-server` の Windows サービス（#392 B3-2b、2026-10-15。設計は
//! docs/chronogazer-service-design.md §3.3・§4 の B3-2b の実装メモ）。**このファイル
//! 全体が Windows だけ**（`chronogazer-server.rs` が `#[cfg(windows)]` で読み込む）。
//!
//! - [`run_service_dispatcher`]（`run-service`）: 共有ランナー
//!   `banto_service_control::run_service` に [`ServerProgram`] を渡す。SCM の外で
//!   実行されたら製品共通の案内 2 行（`dispatch_failure_lines`）を標準エラー出力に
//!   出して終了コード 1。本体はまだ作っていないので、データのルートの下にも何も
//!   作らない。
//! - [`install`]・[`uninstall`]: 共有の手順（`install_service`・`uninstall_service`）
//!   と、インストール利用者の SID の記録（D1・D2）・ルートの用意（D3）。`install` の
//!   本体（`server_service::install_with`）・文言・終了コードは
//!   `chronogazer_core::server_service`（本体は #392 B3-3b で lib へ移し、
//!   `chronogazer-elev.exe service-install` と共有する）。
//!
//! # 本体（[`ServerProgram::run`]）
//!
//! 1. ルートの解決と検証（使えなければ何も作らずに終了コード 3。ログもまだ無い）。
//! 2. 記録したインストール利用者の SID を読む（`Parameters\InstallUserSid`）。
//!    使えなければ `service_only` で続け、ログを開いたら理由と対処を 1 行出す（D4）。
//! 3. tokio のランタイム（`ServiceContext::hold_until_stopped` で預け、Stopped の報告の
//!    後で、残りのタスクを待たずに落とす - コンソールの `mem::forget` と同じ扱い）。
//! 4. 起動の順序はコンソールと同じ `start::start_server`（A1 の排他 → ログ → DB →
//!    Hub のキー → エンジン）。host の種類は `service`（A1 の診断ファイル・
//!    `GET /api/host`・`run\endpoint.json`。シェルはこれで Service の画面へつなぐ）。
//!    段の間で SCM の停止の合図（`stop_notice`）を見て、来ていれば起動を中止して
//!    `ServiceExit::Success`（停止の要求は失敗ではない）。A1 の排他を取れなければ
//!    ログを開かずに終了コード 2（ルートの下に何も作らない。理由はどこにも残らない
//!    - SCM の `SERVICE_EXIT_CODE` とイベントログ 7024 で分かる。B1-5 の約束のまま）。
//! 5. 動き出したら Running を報告し（`report_running`。ログの古いファイルの削除と
//!    待ち受けの案内より先）、止める合図（SCM の STOP・OS の PRESHUTDOWN）を待つ。
//! 6. 後始末: `StartedServer::shutdown`（`run\endpoint.json` を消す → エンジンの終了と
//!    最終 flush）に予算（`server_service::service_stop_budget`。STOP は 15 秒、
//!    PRESHUTDOWN は OS の期限から後の書き出しの 2 秒と余裕の 1 秒を引いた残り）を渡す。エンジンが予算で自分を打ち切る
//!    ので、予算超過の見張り（`OverrunWatch`）は使わない。終了コードは
//!    `server_service::service_exit_for`。
//!
//! **コンソールの Ctrl のハンドラーは登録しない**（`chronogazer-server.rs` の
//! モジュール doc「Windows サービス」）。止める合図は SCM からだけ来る。
//!
//! ログは B1-5 と同じ日次ログ（`<root>\logs\chronogazer-server-YYYY-MM-DD.log`）で、
//! 標準出力・標準エラー出力をそこへ向け直す（Session 0 にはコンソールが無いので、
//! コンソールへの写しは自然に無くなる）。**日次ログを開けなければ起動しない**（終了
//! コード 3。コンソールはコンソールだけで続けるが、サービスでは診断がすべて黙って
//! 失われるため。`open_required_log`、PR #618 の Codex の指摘）。後始末の後の書き出しの
//! 待ちは PRESHUTDOWN の期限までの残りで頭打ちにする
//! （`server_service::service_flush_wait`）。ランナーの `open_log` は使わない（A1 の
//! 排他を取る前にルートの下に何も作らない、B1-5 の約束）。そのため、ランナーが
//! 書く制御ハンドラの登録の失敗の行は、どこにも残らない（既知の制限）。

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use banto_safe_root::InstallUserSid;
use banto_service_control::{
    dispatch_failure_lines, print_console_lines, read_service_parameter, run_service,
    uninstall_service, ServiceContext, ServiceExit, ServiceProgram,
};
use chronogazer_core::instance_lock::ChronoGazerHost;
use chronogazer_core::paths::{resolve_data_root_from_env, DataLayout, DirAccessSource};
use chronogazer_core::server_host::{self, EXIT_START_FAILED, SERVER_LABEL};
use chronogazer_core::server_service::{
    access_from_parameter, install_result_lines, install_with, service_exit_for,
    service_flush_wait, service_running_line, service_starting_line, service_stop_budget,
    service_stop_done_line, service_stop_hint_line, service_stop_reason_line, service_stopped_line,
    stop_signal_label, uninstall_lines, INSTALL_USER_SID_VALUE, SERVICE_EXPECTED_STOP,
    SERVICE_LOG_FLUSH_WAIT,
};
use chronogazer_core::shell_launch::SERVICE_NAME;

use crate::start::{self, StartHooks, StartSpec, Started};
use crate::{Flush, LineSink, OpenedLog};

/// データのルートを決める（本番は `start::resolve_layout`。テストは一時ルート）。
type LayoutSource = Box<dyn Fn() -> Result<DataLayout, i32> + Send + Sync>;
/// 記録したインストール利用者の SID を読む（本番は `Parameters`。テストは偽物 -
/// テストは HKLM を読み書きしない）。
type InstallUserReader = Box<dyn Fn() -> Result<Option<String>, String> + Send + Sync>;
/// 日次ログを開いて向け直す（本番は `crate::open_log`。テストはテストのプロセスの
/// 標準ハンドルを差し替えない偽物）。
type LogOpener = Box<dyn Fn(&DataLayout) -> Result<OpenedLog, String> + Send + Sync>;

/// `run-service`: SCM のディスパッチャを起動して [`ServerProgram`] を動かす。SCM の外
/// （人が手で実行した）なら案内の 2 行を出して 1。
pub(crate) fn run_service_dispatcher() -> i32 {
    match run_service(ServerProgram::production()) {
        Ok(()) => server_host::EXIT_OK,
        Err(err) => {
            for line in dispatch_failure_lines(SERVER_LABEL, &err) {
                eprintln!("{line}");
            }
            1
        }
    }
}

/// ChronoGazer のサービス本体（共有ランナーに渡す）。SCM とのやりとり（制御ハンドラ、
/// Running → StopPending → Stopped の報告、panic）は共有ランナーが持つ。
pub(crate) struct ServerProgram {
    layout: LayoutSource,
    read_install_user: InstallUserReader,
    open_log: LogOpener,
    /// ログを開いた後の、向け直しの後始末（[`ServerProgram::flush`]）。
    flush: Mutex<Option<Flush>>,
    /// ログを開いた後の、パイプを通さない記録先（[`ServiceProgram::log_error`]）。
    record: Mutex<Option<LineSink>>,
}

impl ServerProgram {
    fn production() -> Self {
        Self::new(
            Box::new(start::resolve_layout),
            Box::new(|| {
                read_service_parameter(SERVICE_NAME, INSTALL_USER_SID_VALUE)
                    .map_err(|err| err.to_string())
            }),
            Box::new(open_required_log),
        )
    }

    fn new(
        layout: LayoutSource,
        read_install_user: InstallUserReader,
        open_log: LogOpener,
    ) -> Self {
        Self {
            layout,
            read_install_user,
            open_log,
            flush: Mutex::new(None),
            record: Mutex::new(None),
        }
    }

    /// ログの書き出しを `wait` まで待つ（ログを開いていなければ何もしない）。
    fn flush(&self, wait: Duration) {
        if let Some(flush) = self
            .flush
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .as_ref()
        {
            flush(wait);
        }
    }

    fn run_inner(&self, ctx: &ServiceContext) -> ServiceExit {
        ctx.set_expected_stop(SERVICE_EXPECTED_STOP);

        // 1. ルート（使えなければ理由は標準エラー出力だけ。ログはまだ無い）。
        let layout = match (self.layout)() {
            Ok(layout) => layout,
            Err(code) => return service_exit_for(code),
        };

        // 2. インストール利用者（D1・D4）。
        let read = (self.read_install_user)();
        let (access, access_warning) = access_from_parameter(match &read {
            Ok(value) => Ok(value.as_deref()),
            Err(err) => Err(err.as_str()),
        });

        // 3. ランタイム（Stopped の報告の後で、残りのタスクを待たずに落とす）。
        let runtime = match tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
        {
            Ok(runtime) => ctx.hold_until_stopped(BackgroundRuntime(Some(runtime))),
            Err(err) => {
                eprintln!("{SERVER_LABEL}: 非同期ランタイムを作れませんでした: {err}");
                return service_exit_for(EXIT_START_FAILED);
            }
        };

        // 4. 起動の順序（コンソールと共有）。
        let hooks = ServiceHooks {
            program: self,
            ctx,
            access_warning,
            aborted: AtomicBool::new(false),
        };
        let started = start::start_server(
            &runtime,
            StartSpec {
                layout: &layout,
                host: ChronoGazerHost::Service,
                access: &DirAccessSource::Given(access),
            },
            &hooks,
        );
        let Started { server, log } = match started {
            Ok(started) => started,
            // 起動の途中で止める合図を受けた: 起動を中止しただけで、失敗ではない。
            Err(_) if hooks.aborted.load(Ordering::SeqCst) => return ServiceExit::Success,
            Err(code) => return service_exit_for(code),
        };

        // 5. 動いていることを報告してから、上限の無い処理（古いログの削除・案内）へ。
        let log_prune = log.as_ref().and_then(|log| {
            log.lock()
                .ok()
                .and_then(|mut log| log.set_retention(server.log_retention_days()))
        });
        if ctx.report_running() {
            println!("{}", service_running_line());
        }
        if let Some(prune) = log_prune {
            crate::spawn_log_prune(prune);
        }
        if ctx.stop_notice().is_none() {
            runtime.block_on(crate::announce(&server, &service_stop_hint_line()));
        }

        // 6. 止める合図を待って後始末。
        let notice = ctx.wait_for_stop();
        let budget = service_stop_budget(&notice, Instant::now());
        println!("{}", service_stop_reason_line(notice.signal, budget));
        let summary = runtime.block_on(server.shutdown(budget));
        // #599: endpoint.json を残したこと（終了コードは変えない）。
        if let Some(line) = summary.endpoint_diagnostic() {
            eprintln!("{line}");
        }
        if let Some(err) = &summary.report.collect_stop_error {
            eprintln!("{SERVER_LABEL}: 最終の書き出しに失敗しました: {err}");
        }
        // 予算切れはエンジンが自分で 1 行出している。
        if !summary.report.timed_out {
            println!("{}", service_stop_done_line(notice.signal));
        }
        let exit = service_exit_for(summary.exit_code());
        println!("{}", service_stopped_line(exit));
        // 書き出しの待ちも PRESHUTDOWN の期限までの残りで頭打ち（PR #618 の Codex の指摘）。
        self.flush(service_flush_wait(&notice, Instant::now()));
        exit
    }
}

impl ServiceProgram for ServerProgram {
    fn service_name(&self) -> &str {
        SERVICE_NAME
    }

    fn label(&self) -> &str {
        SERVER_LABEL
    }

    // `open_log` は既定（何もしない）: ログは A1 の排他を取った後に開く（B1-5）。

    fn log_error(&self, line: &str) {
        let record = self
            .record
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        match record {
            Some(record) => record(line),
            None => eprintln!("{line}"),
        }
    }

    fn run(&self, ctx: &ServiceContext) -> ServiceExit {
        self.run_inner(ctx)
    }
}

/// サービスの起動の順序の差し込み（`start::start_server`）。
struct ServiceHooks<'a> {
    program: &'a ServerProgram,
    ctx: &'a ServiceContext,
    /// ログを開いたら出す、インストール利用者の記録が使えない理由と対処（D4）。
    access_warning: Option<String>,
    /// 段の間で止める合図を見つけた（起動の中止。失敗ではない）。
    aborted: AtomicBool,
}

impl StartHooks for ServiceHooks<'_> {
    fn stop_requested(&self) -> Option<&'static str> {
        let notice = self.ctx.stop_notice()?;
        // 合図を返すと `start_server` は必ず起動を中止する。
        self.aborted.store(true, Ordering::SeqCst);
        Some(stop_signal_label(notice.signal))
    }

    fn open_log(&self, layout: &DataLayout) -> Result<OpenedLog, String> {
        (self.program.open_log)(layout)
    }

    fn log_opened(&self, flush: Flush, record: Option<LineSink>) {
        *self.program.flush.lock().unwrap_or_else(|p| p.into_inner()) = Some(flush);
        *self
            .program
            .record
            .lock()
            .unwrap_or_else(|p| p.into_inner()) = record;
        println!("{}", service_starting_line());
        if let Some(warning) = &self.access_warning {
            eprintln!("{warning}");
        }
    }

    fn finish_start_failure(&self, code: i32) -> i32 {
        if self.aborted.load(Ordering::SeqCst) {
            println!("{}", service_stopped_line(ServiceExit::Success));
        } else {
            eprintln!("{SERVER_LABEL}: Windows サービスの起動に失敗しました（終了コード {code}）");
        }
        // 起動の途中で PRESHUTDOWN を受けていれば、その期限までの残りで頭打ち。
        let wait = self
            .ctx
            .stop_notice()
            .map_or(SERVICE_LOG_FLUSH_WAIT, |notice| {
                service_flush_wait(&notice, Instant::now())
            });
        self.program.flush(wait);
        code
    }
}

/// サービスの日次ログを開く（PR #618 の Codex の指摘 P2）。コンソールの `crate::open_log`
/// は、ログのファイルを開けない（`logs\` に書けない・ディスクが無い）ときコンソール
/// だけで続ける（`Ok((None, ..))`）が、サービス（Session 0）にはコンソールが無く、
/// そのまま動くと起動・収集・停止の診断がすべて黙って失われる。サービスではそれを
/// 起動の失敗にする（`start::start_server` が終了コード 3 で止め、Running を報告
/// しない）。残るのは SCM の終了コードと、標準エラー出力（Session 0 では見えない）だけ。
fn open_required_log(layout: &DataLayout) -> Result<OpenedLog, String> {
    require_log(crate::open_log(layout)?, layout)
}

/// 開けたログが無ければ起動を止める理由（[`open_required_log`]）。
fn require_log(opened: OpenedLog, layout: &DataLayout) -> Result<OpenedLog, String> {
    if opened.0.is_none() {
        return Err(format!(
            "日次ログ（{}）を開けませんでした。Windows サービスにはコンソールが無く、起動・収集・停止の診断が残らないため、起動しません",
            layout.logs_dir().display()
        ));
    }
    Ok(opened)
}

/// tokio のランタイムを、drop のときに残りのタスクを待たずに落とす入れ物
/// （`shutdown_background`）。止めたエンジンの残りのタスクで、Stopped を報告した後の
/// プロセスの終わりを待たせない（コンソールの `mem::forget(runtime)` と同じ扱い）。
struct BackgroundRuntime(Option<tokio::runtime::Runtime>);

impl std::ops::Deref for BackgroundRuntime {
    type Target = tokio::runtime::Runtime;

    fn deref(&self) -> &Self::Target {
        self.0.as_ref().expect("drop の前だけ使う")
    }
}

impl Drop for BackgroundRuntime {
    fn drop(&mut self) {
        if let Some(runtime) = self.0.take() {
            runtime.shutdown_background();
        }
    }
}

// --- 登録・解除 ---------------------------------------------------------------

/// `install`: サービスを登録し、インストール利用者の SID を記録し（D1・D2）、ルートが
/// 無ければその SID で作る（D3）。終了コード（失敗は 1）を返す。本体は
/// `server_service::install_with`（#392 B3-3b で lib へ移した。`chronogazer-elev.exe
/// service-install` と共有）。ここで決めるのは、登録する実行ファイル（自分）・
/// データのルートと Program Files の場所（このプロセスの環境）だけで、出す行と
/// 終了コードは移す前と同じ。
pub(crate) fn install(install_user_sid: Option<InstallUserSid>) -> i32 {
    let executable = match std::env::current_exe() {
        Ok(path) => path,
        Err(err) => {
            eprintln!("{SERVER_LABEL}: 自身の実行ファイルパスの取得に失敗しました: {err}");
            return 1;
        }
    };
    let result = install_with(
        executable,
        install_user_sid,
        &DataLayout::from_env(),
        std::env::var_os("ProgramFiles").map(PathBuf::from),
    );
    let (lines, code) = install_result_lines(&result);
    print_console_lines(&lines);
    code
}

/// `uninstall`: 登録を解除する（`Parameters` もサービスのキーと一緒に消える）。データは
/// 残す。終了コード（失敗は 1）を返す。
pub(crate) fn uninstall() -> i32 {
    let result = uninstall_service(SERVICE_NAME);
    print_console_lines(&uninstall_lines(&result, &resolve_data_root_from_env()));
    i32::from(result.is_err())
}

#[cfg(test)]
mod tests {
    //! サービス本体のテスト。**SCM も HKLM も使わない**: 共有ランナーの芯
    //! （`run_program_with`）に偽の登録を渡し、制御はテストから送る。ルートは一時
    //! ディレクトリ、インストール利用者の記録は偽の読み取り、ログはテストのプロセスの
    //! 標準ハンドルを差し替えない偽物（`logs\` を作るだけ）。`install`・`uninstall` は
    //! 走らせない（GitHub の Windows のランナーは管理者で、本当に登録してしまう）。

    use super::*;
    use std::sync::{mpsc, Arc};

    use banto_collect::{simulation, Protocol};
    use banto_safe_root::acl::{current_user_sid, read_dir_dacl, reset_dir_acl};
    use banto_safe_root::DirAccess;
    use banto_service_control::{
        run_program_with, ControlHandler, ControlReply, ErrorLog, ScmControlKind, ServicePhase,
        ServiceStatusPlan,
    };
    use banto_tags::{
        CollectionGroupInput, CollectionGroupService, PlcConnectionInput, PlcConnectionService,
        TagInput, TagService,
    };
    use banto_tstore::{list_data_files, TsReader};
    use chronogazer_core::collect::{CollectEventRow, CollectorService, EventPage, Readout};
    use chronogazer_core::db::{init_db, Db};
    use chronogazer_core::endpoint::read_endpoint;
    use chronogazer_core::settings::SettingsService;

    const WAIT_LIMIT: Duration = Duration::from_secs(40);

    // `install` の記録（`record_install_user`）とルートの用意（`prepare_root`）の
    // テストは、本体と一緒に #392 B3-3b で `chronogazer_core::server_service` へ移した。

    // --- サービス本体（偽の登録） ------------------------------------------------

    type Seen = Arc<Mutex<Vec<(ServiceStatusPlan, ServiceExit)>>>;
    /// 偽の登録が返す、報告を記録する関数。
    type TestReport = Box<dyn Fn(&ServiceStatusPlan, ServiceExit) + Send + Sync>;

    /// 偽の登録: ハンドラをテストへ渡し、報告を記録する（共有 crate の芯のテストと
    /// 同じ形）。
    fn fake_register(
        handler_tx: mpsc::Sender<ControlHandler>,
        seen: Seen,
    ) -> impl FnOnce(ControlHandler, ErrorLog) -> Result<TestReport, String> {
        move |handler, _log| {
            handler_tx.send(handler).unwrap();
            Ok(Box::new(move |plan: &ServiceStatusPlan, exit| {
                seen.lock().unwrap().push((*plan, exit))
            }))
        }
    }

    fn phases(seen: &Seen) -> Vec<(ServicePhase, ServiceExit)> {
        seen.lock()
            .unwrap()
            .iter()
            .map(|(plan, exit)| (plan.phase, *exit))
            .collect()
    }

    /// 動いているサービス（別スレッドの `run_program_with`）。
    struct Running {
        handler: ControlHandler,
        seen: Seen,
        thread: Option<std::thread::JoinHandle<Option<ServiceExit>>>,
    }

    impl Running {
        fn send(&self, control: ScmControlKind) {
            assert_eq!((self.handler)(control), ControlReply::NoError);
        }

        fn join(mut self) -> Option<ServiceExit> {
            self.thread.take().unwrap().join().unwrap()
        }
    }

    impl Drop for Running {
        fn drop(&mut self) {
            // テストが途中で失敗しても、エンジンを止めてから抜ける。
            if let Some(thread) = self.thread.take() {
                let _ = (self.handler)(ScmControlKind::Stop);
                let _ = thread.join();
            }
        }
    }

    /// テスト用の本体: ルートは `layout`、記録は `install_user`、ログは `logs\` を作る
    /// だけ（呼ばれたら `log_opened` を立てる）。
    fn test_program(
        layout: &DataLayout,
        install_user: Result<Option<String>, String>,
        log_opened: Arc<AtomicBool>,
    ) -> ServerProgram {
        let layout = layout.clone();
        ServerProgram::new(
            Box::new(move || Ok(layout.clone())),
            Box::new(move || install_user.clone()),
            Box::new(move |layout: &DataLayout| {
                log_opened.store(true, Ordering::SeqCst);
                std::fs::create_dir_all(layout.logs_dir()).map_err(|err| err.to_string())?;
                Ok((None, Box::new(|_| {}), None))
            }),
        )
    }

    fn spawn(program: ServerProgram) -> Running {
        let (handler_tx, handler_rx) = mpsc::channel();
        let seen: Seen = Arc::new(Mutex::new(Vec::new()));
        let thread = {
            let seen = seen.clone();
            std::thread::spawn(move || run_program_with(program, fake_register(handler_tx, seen)))
        };
        let handler = handler_rx
            .recv_timeout(Duration::from_secs(10))
            .expect("制御ハンドラが登録されない");
        Running {
            handler,
            seen,
            thread: Some(thread),
        }
    }

    async fn wait_for<T>(what: &str, mut probe: impl AsyncFnMut() -> Option<T>) -> T {
        let deadline = Instant::now() + WAIT_LIMIT;
        loop {
            if let Some(value) = probe().await {
                return value;
            }
            assert!(Instant::now() < deadline, "{what} が成り立たなかった");
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    async fn wait_running(running: &Running) {
        wait_for("Running の報告", async || {
            phases(&running.seen)
                .iter()
                .any(|(phase, _)| *phase == ServicePhase::Running)
                .then_some(())
        })
        .await;
    }

    /// 一時ルートの DB を作り、待ち受けを port 0 にする。`plc_port` があれば、
    /// シミュレータの Modbus TCP の接続・グループ（100ms）・タグを 1 つずつ登録する。
    async fn prepare_root_db(layout: &DataLayout, plc_port: Option<u16>) {
        std::fs::create_dir_all(layout.root()).unwrap();
        let pool = init_db(layout.db_path()).await.expect("init_db");
        SettingsService::new(Db::Sqlite(pool.clone()))
            .set("server.port", "0")
            .await
            .unwrap();
        if let Some(port) = plc_port {
            let conn = PlcConnectionService::new(pool.clone())
                .create(PlcConnectionInput {
                    name: "service-modbus".to_string(),
                    protocol: "modbus-tcp".to_string(),
                    host: "127.0.0.1".to_string(),
                    port: i64::from(port),
                    unit_id: 1,
                    enabled: true,
                    simulation: false,
                    word_order: String::new(),
                    cpu_type: None,
                    database: None,
                    username: None,
                    password: None,
                })
                .await
                .expect("接続");
            let group = CollectionGroupService::new(pool.clone())
                .create(CollectionGroupInput {
                    name: "service-group".to_string(),
                    plc_connection_id: conn.id,
                    period_ms: 100,
                    enabled: true,
                    default_writable: false,
                    query_sql: None,
                })
                .await
                .expect("グループ");
            TagService::new(pool.clone())
                .create(TagInput {
                    name: "値".to_string(),
                    collection_group_id: group.id,
                    address: "40001".to_string(),
                    data_type: "u16".to_string(),
                    string_length: None,
                    string_encoding: "utf8".to_string(),
                    raw_lo: None,
                    raw_hi: None,
                    eng_lo: None,
                    eng_hi: None,
                    unit: None,
                    decimals: 0,
                    enabled: true,
                    writable: false,
                    tag_kind: "plc".to_string(),
                    expression: None,
                    retain: false,
                    expected_revision: None,
                })
                .await
                .expect("タグ");
        }
        pool.close().await;
    }

    async fn latest_event(layout: &DataLayout) -> Option<String> {
        let pool = init_db(layout.db_path()).await.expect("開き直す");
        let reader = CollectorService::new(pool.clone(), layout.data_dir());
        let rows: Vec<CollectEventRow> = match reader.events(EventPage::default()).await {
            Readout::Ready { data } => data.rows,
            other => panic!("イベントが読めない: {}", other.as_str()),
        };
        pool.close().await;
        rows.first().map(|row| row.kind.clone())
    }

    async fn stored_row_count(layout: &DataLayout) -> usize {
        let mut count = 0;
        for file in list_data_files(&layout.data_dir()).expect("データファイルの列挙") {
            let reader = TsReader::open(&file.path).await.expect("開ける");
            for group in reader.groups() {
                count += reader
                    .read_range(&group.key, i64::MIN, i64::MAX)
                    .await
                    .expect("読める")
                    .len();
            }
        }
        count
    }

    async fn get_json(url: String, token: Option<&str>) -> serde_json::Value {
        let mut request = reqwest::Client::new()
            .get(url)
            .header("X-Banto-Client", "banto");
        if let Some(token) = token {
            request = request.header("Authorization", format!("Bearer {token}"));
        }
        let response = request.send().await.expect("send");
        serde_json::from_str(&response.text().await.unwrap_or_default()).unwrap_or_default()
    }

    /// この PC からは最初の管理者を作れる（token が返る）。
    async fn setup_admin(port: u16) -> String {
        let response = reqwest::Client::new()
            .post(format!("http://127.0.0.1:{port}/api/auth/setup"))
            .header("X-Banto-Client", "banto")
            .header("Content-Type", "application/json")
            .body(
                serde_json::json!({"username":"admin","password":"admin-pass-1","displayName":"管理者"})
                    .to_string(),
            )
            .send()
            .await
            .expect("send");
        assert_eq!(response.status(), 200);
        let body: serde_json::Value =
            serde_json::from_str(&response.text().await.unwrap()).unwrap();
        body["token"].as_str().expect("token").to_string()
    }

    /// 収集が走り、現在値に good の値が出るまで待つ。
    async fn wait_collecting(port: u16, token: &str) {
        wait_for("収集の現在値", async || {
            let state = get_json(format!("http://127.0.0.1:{port}/api/collect"), Some(token)).await;
            if state["state"] != "running" {
                return None;
            }
            let values = get_json(
                format!("http://127.0.0.1:{port}/api/collect/values"),
                Some(token),
            )
            .await;
            values.to_string().contains("\"good\"").then_some(())
        })
        .await;
    }

    /// Running の報告は 1 回、SCM の STOP で最終 flush をして Stopped(0)。host の種類は
    /// `service`（`endpoint.json` と `GET /api/host`）、ACL の利用者は記録した SID、
    /// コンソールの Ctrl のハンドラーは登録しない。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn runs_as_a_service_and_flushes_on_scm_stop() {
        let dir = tempfile::tempdir().unwrap();
        let layout = DataLayout::new(dir.path().join("root"));
        let sim = simulation::start(Protocol::ModbusTcp).await;
        prepare_root_db(&layout, Some(sim.addr().port())).await;
        let me = current_user_sid().unwrap();
        let log_opened = Arc::new(AtomicBool::new(false));
        let running = spawn(test_program(
            &layout,
            Ok(Some(me.as_str().to_string())),
            log_opened.clone(),
        ));
        wait_running(&running).await;

        let endpoint = read_endpoint(&layout.endpoint_file()).expect("endpoint.json");
        assert_eq!(endpoint.host_kind, "service");
        assert_eq!(endpoint.pid, std::process::id());
        let host = get_json(format!("http://127.0.0.1:{}/api/host", endpoint.port), None).await;
        assert_eq!(host["hostKind"], "service", "{host}");
        assert_eq!(host["product"], "chronogazer", "{host}");
        let token = setup_admin(endpoint.port).await;
        wait_collecting(endpoint.port, &token).await;

        // サービスの本体はコンソールの Ctrl のハンドラーを登録しない（登録すると、
        // ログオフの `CTRL_LOGOFF_EVENT` でサービスが止まる）。
        assert!(
            !crate::console::ctrl_handler_installed(),
            "サービスの本体がコンソールの Ctrl のハンドラーを登録した"
        );
        assert!(log_opened.load(Ordering::SeqCst));
        // Hub のキーの保存先は記録した SID（ここではテストの利用者）を利用者の欄に作る。
        let dacl = read_dir_dacl(&layout.hub_keys_dir()).unwrap();
        assert!(dacl
            .aces
            .as_ref()
            .unwrap()
            .iter()
            .any(|ace| ace.sid == me.as_str()));

        running.send(ScmControlKind::Stop);
        let seen = running.seen.clone();
        assert_eq!(running.join(), Some(ServiceExit::Success));
        let phases = phases(&seen);
        assert_eq!(
            phases
                .iter()
                .filter(|(phase, _)| *phase == ServicePhase::Running)
                .count(),
            1,
            "{phases:?}"
        );
        assert!(
            phases
                .iter()
                .any(|(phase, _)| *phase == ServicePhase::StopPending),
            "{phases:?}"
        );
        assert_eq!(
            phases.last(),
            Some(&(ServicePhase::Stopped, ServiceExit::Success))
        );
        drop(sim);

        assert!(!layout.endpoint_file().exists(), "endpoint.json が残った");
        assert_eq!(
            latest_event(&layout).await.as_deref(),
            Some("collection_stopped")
        );
        assert!(stored_row_count(&layout).await > 0, "行が残っていない");
    }

    /// A1 の排他を別のプロセス（ここではデスクトップの印）が持っていれば、ログを開かず・
    /// ルートの下に何も作らずに Stopped(2)。Running は報告しない。
    #[test]
    fn a_held_instance_lock_stops_with_2_without_creating_anything() {
        let dir = tempfile::tempdir().unwrap();
        let layout = DataLayout::new(dir.path());
        let _desktop = chronogazer_core::instance_lock::acquire_for_db(
            &layout.db_path(),
            ChronoGazerHost::Desktop,
        )
        .unwrap();
        let log_opened = Arc::new(AtomicBool::new(false));
        let running = spawn(test_program(
            &layout,
            Ok(Some(current_user_sid().unwrap().as_str().to_string())),
            log_opened.clone(),
        ));
        let seen = running.seen.clone();
        assert_eq!(running.join(), Some(ServiceExit::Specific(2)));
        assert_eq!(
            phases(&seen),
            vec![(ServicePhase::Stopped, ServiceExit::Specific(2))]
        );
        assert!(!log_opened.load(Ordering::SeqCst), "ログを開いた");
        for path in [
            layout.db_path(),
            layout.logs_dir(),
            layout.run_dir(),
            layout.data_dir(),
            layout.secrets_dir(),
        ] {
            assert!(!path.exists(), "{} を作った", path.display());
        }
    }

    /// PR #618 の Codex の指摘: 日次ログを開けない（`logs` がファイルで、ディレクトリを
    /// 作れない）ときは、コンソールのように黙って続けず、Running を報告せずに
    /// Stopped(3)。本番と同じ開き方（`open_required_log`）を通す（開けないので
    /// テストのプロセスの標準ハンドルは差し替わらない）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_unopenable_daily_log_stops_with_3_before_running() {
        let dir = tempfile::tempdir().unwrap();
        let layout = DataLayout::new(dir.path().join("root"));
        prepare_root_db(&layout, None).await;
        std::fs::write(layout.logs_dir(), b"not a directory").unwrap();
        let program = ServerProgram::new(
            Box::new({
                let layout = layout.clone();
                move || Ok(layout.clone())
            }),
            Box::new(|| Ok(Some(current_user_sid().unwrap().as_str().to_string()))),
            Box::new(open_required_log),
        );
        let running = spawn(program);
        let seen = running.seen.clone();
        // Running か Stopped のどちらかが報告されるまで待つ（Running になったら、
        // `Running` の drop が STOP を送って止める）。
        wait_for("Running か Stopped の報告", async || {
            phases(&seen)
                .iter()
                .any(|(phase, _)| matches!(phase, ServicePhase::Running | ServicePhase::Stopped))
                .then_some(())
        })
        .await;
        assert!(
            !phases(&seen)
                .iter()
                .any(|(phase, _)| *phase == ServicePhase::Running),
            "ログを開けないのに Running を報告した: {:?}",
            phases(&seen)
        );
        assert_eq!(running.join(), Some(ServiceExit::Specific(3)));
        assert_eq!(
            phases(&seen),
            vec![(ServicePhase::Stopped, ServiceExit::Specific(3))]
        );
        assert!(!layout.endpoint_file().exists(), "endpoint.json を書いた");
    }

    /// OS の PRESHUTDOWN: OS の期限（受けてから 27 秒）より前に Stopped(0) まで終わる。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn preshutdown_finishes_within_the_os_budget() {
        let dir = tempfile::tempdir().unwrap();
        let layout = DataLayout::new(dir.path().join("root"));
        prepare_root_db(&layout, None).await;
        let running = spawn(test_program(
            &layout,
            Ok(Some(current_user_sid().unwrap().as_str().to_string())),
            Arc::new(AtomicBool::new(false)),
        ));
        wait_running(&running).await;
        let received = Instant::now();
        running.send(ScmControlKind::Preshutdown);
        let seen = running.seen.clone();
        assert_eq!(running.join(), Some(ServiceExit::Success));
        let elapsed = received.elapsed();
        assert!(
            elapsed
                < banto_service_control::stop_work_budget(
                    banto_service_control::PRESHUTDOWN_TIMEOUT
                ),
            "{elapsed:?}"
        );
        assert_eq!(
            phases(&seen).last(),
            Some(&(ServicePhase::Stopped, ServiceExit::Success))
        );
        assert!(!layout.endpoint_file().exists());
    }

    /// D4: インストール利用者の記録が無くても起動を続ける（`service_only`）。Hub の
    /// キーの保存先に利用者の ACE を付けない。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_missing_install_user_record_falls_back_to_service_only() {
        let dir = tempfile::tempdir().unwrap();
        let layout = DataLayout::new(dir.path().join("root"));
        prepare_root_db(&layout, None).await;
        let running = spawn(test_program(
            &layout,
            Ok(None),
            Arc::new(AtomicBool::new(false)),
        ));
        wait_running(&running).await;
        running.send(ScmControlKind::Stop);
        assert_eq!(running.join(), Some(ServiceExit::Success));

        let me = current_user_sid().unwrap();
        let hub_keys = layout.hub_keys_dir();
        // 起動は Hub のキーの保存先を必ず作る（`open_key_store_with` → `FileSecretStore::open`）。
        assert!(hub_keys.exists(), "Hub のキーの保存先を作っていない");
        let dacl = read_dir_dacl(&hub_keys).unwrap();
        assert!(
            dacl.aces
                .as_ref()
                .unwrap()
                .iter()
                .all(|ace| ace.sid != me.as_str()),
            "記録が無いのに利用者の ACE を付けた: {dacl:?}"
        );
        // 片付け（所有者は自分なので書き換えられる）。
        reset_dir_acl(&hub_keys, &DirAccess::with_install_user(me)).unwrap();
    }

    /// 起動の途中（DB を開く前）に STOP を受けたら、起動を中止して Stopped(0)（失敗では
    /// ない）。Running は報告しない。
    #[test]
    fn a_stop_during_the_start_aborts_it_as_success() {
        let dir = tempfile::tempdir().unwrap();
        let layout = DataLayout::new(dir.path().join("root"));
        let (opened_tx, opened_rx) = mpsc::channel::<()>();
        let (go_tx, go_rx) = mpsc::channel::<()>();
        let go_rx = Mutex::new(go_rx);
        let program = ServerProgram::new(
            Box::new({
                let layout = layout.clone();
                move || Ok(layout.clone())
            }),
            Box::new(|| Ok(Some(current_user_sid().unwrap().as_str().to_string()))),
            // ログを開いたところで止まり、テストが STOP を送ってから進む。
            Box::new(move |_layout: &DataLayout| {
                let _ = opened_tx.send(());
                let _ = go_rx.lock().unwrap().recv_timeout(Duration::from_secs(20));
                Ok((None, Box::new(|_| {}), None))
            }),
        );
        let running = spawn(program);
        opened_rx
            .recv_timeout(Duration::from_secs(20))
            .expect("ログを開くところまで来ない");
        running.send(ScmControlKind::Stop);
        go_tx.send(()).unwrap();
        let seen = running.seen.clone();
        assert_eq!(running.join(), Some(ServiceExit::Success));
        let phases = phases(&seen);
        assert!(
            !phases
                .iter()
                .any(|(phase, _)| *phase == ServicePhase::Running),
            "{phases:?}"
        );
        assert_eq!(
            phases.last(),
            Some(&(ServicePhase::Stopped, ServiceExit::Success))
        );
        assert!(!layout.db_path().exists(), "DB を開いた");
    }
}
