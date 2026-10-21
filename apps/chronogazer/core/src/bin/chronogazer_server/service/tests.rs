//! サービス本体のテスト（#392 B3-2b、#623 PR-3 で設定モード ⇔ 運用モードの順序を足した）。
//! **SCM も HKLM も使わない**: 共有ランナーの芯（`run_program_with_args`）に偽の登録を渡し、
//! 制御はテストから送る。ルートは一時ディレクトリ、インストール利用者の記録は偽の読み取り、
//! ログはテストのプロセスの標準ハンドルを差し替えない偽物（本体が書く行を集める）。
//! ACL の付け替えの部品（[`AclOps`]）は偽物（[`FakeAcl`]）で、ルートは `Mode::Plain`
//! （`std::fs`。どの OS・権限でも動く）。偽物は呼ばれた順を本体のログの行と**同じ列**に
//! 記録するので、「締め終わる前にエンジンを動かさない」「緩め始めた後に書かない」を
//! 列の順序で確かめられる。本物の締める・緩めるは `banto-safe-root` の実物テスト（CI の
//! 管理者）。`install`・`uninstall` は走らせない（GitHub の Windows のランナーは管理者で、
//! 本当に登録してしまう）。

use super::*;
use std::ffi::OsString;
use std::sync::atomic::AtomicUsize;

use banto_collect::{simulation, Protocol};
use banto_safe_root::mode::TreeProblem;
use banto_service_control::{
    run_program_with_args, ControlHandler, ControlReply, ErrorLog, ScmControlKind, ServicePhase,
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
/// 本体が終わるのを待つ上限（[`Running::join`]）。退行して本体が止まらなくなっても（知らない
/// 引数を通常の開始にしてしまう等）、CI のジョブをハングさせずにはっきり落とすため。
const JOIN_LIMIT: Duration = Duration::from_secs(60);
/// 偽の記録（D1）に使う SID（HKLM は読まない）。
const USER: &str = "S-1-5-21-1111111111-2222222222-3333333333-1001";
/// 偽の `relax_tree` の中で待たせる上限（テストが放し忘れても本体を止めたままにしない）。
const RELAX_HOLD_LIMIT: Duration = Duration::from_secs(30);

/// 本体の行と偽の部品の記録を 1 つに集める列。
type Trace = Arc<Mutex<Vec<String>>>;

fn push(trace: &Trace, line: impl Into<String>) {
    trace.lock().unwrap().push(line.into());
}

fn snapshot(trace: &Trace) -> Vec<String> {
    trace.lock().unwrap().clone()
}

/// `needle` を含む最初の行の位置。
fn index_of(lines: &[String], needle: &str) -> Option<usize> {
    lines.iter().position(|line| line.contains(needle))
}

fn position(lines: &[String], needle: &str) -> usize {
    index_of(lines, needle).unwrap_or_else(|| panic!("{needle:?} が無い: {lines:#?}"))
}

/// 偽の記録の行（`acl:` で始まる）だけ。
fn acl_calls(lines: &[String]) -> Vec<String> {
    lines
        .iter()
        .filter(|line| line.starts_with("acl:"))
        .cloned()
        .collect()
}

// --- 偽の部品 ----------------------------------------------------------------------

/// [`AclOps`] の偽物。`Mode::Plain` のルートの上で、呼ばれた順を記録し、段ごとの失敗を注入する。
#[derive(Default)]
struct FakeAcl {
    trace: Trace,
    /// 1 周目の結果（`None` なら通る）。
    scan: Mutex<Option<ScanRefusal>>,
    /// 2 周目で止まる理由（`None` なら通る）。
    close: Mutex<Option<AbortReason>>,
    /// 緩めるのを失敗させる。
    relax_fails: AtomicBool,
    /// この key の印を失敗させる。
    marker: Mutex<Option<(String, MarkerError)>>,
    /// この回数目の確かめ直しから失敗させる（`0` は失敗させない）。
    recheck_fails_at: AtomicUsize,
    recheck_calls: AtomicUsize,
    /// 緩めるの中で待たせる（`acl:relax` を記録した後、入ったことを 1 つ目で知らせ、2 つ目で
    /// 放されるまで待つ。上限は [`RELAX_HOLD_LIMIT`]）。
    relax_hold: Mutex<Option<(mpsc::Sender<()>, mpsc::Receiver<()>)>>,
}

impl FakeAcl {
    fn new(trace: Trace) -> Arc<Self> {
        Arc::new(Self {
            trace,
            ..Self::default()
        })
    }
}

fn tick(progress: &mut dyn FnMut(&Progress)) {
    progress(&Progress {
        examined: 3,
        written: 3,
    });
}

impl AclOps for FakeAcl {
    fn root_mode(&self) -> Mode {
        Mode::Plain
    }

    fn scan_tree(
        &self,
        _root: &SafeRoot,
        stop: &dyn Fn() -> TreeStop,
        progress: &mut dyn FnMut(&Progress),
    ) -> Result<ScanReport, ScanRefusal> {
        push(&self.trace, "acl:scan");
        tick(progress);
        if let Some(refusal) = self.scan.lock().unwrap().clone() {
            return Err(refusal);
        }
        match stop() {
            TreeStop::None => Ok(ScanReport { examined: 3 }),
            signal => Err(ScanRefusal::Stopped(signal)),
        }
    }

    fn close_tree(
        &self,
        _root: &SafeRoot,
        _mode: &ModeAccess,
        stop: &dyn Fn() -> TreeStop,
        progress: &mut dyn FnMut(&Progress),
    ) -> Result<Progress, CloseAbort> {
        push(&self.trace, "acl:close");
        tick(progress);
        if let Some(reason) = self.close.lock().unwrap().clone() {
            return Err(CloseAbort { written: 1, reason });
        }
        match stop() {
            TreeStop::None => Ok(Progress {
                examined: 3,
                written: 3,
            }),
            signal => Err(CloseAbort {
                written: 1,
                reason: AbortReason::Stopped(signal),
            }),
        }
    }

    fn relax_tree(
        &self,
        _root: &SafeRoot,
        _mode: &ModeAccess,
        progress: &mut dyn FnMut(&Progress),
    ) -> Result<RelaxReport, SafeRootError> {
        push(&self.trace, "acl:relax");
        tick(progress);
        let hold = self.relax_hold.lock().unwrap().take();
        if let Some((entered, release)) = hold {
            let _ = entered.send(());
            let _ = release.recv_timeout(RELAX_HOLD_LIMIT);
        }
        if self.relax_fails.load(Ordering::SeqCst) {
            return Err(SafeRootError::UnsupportedPlatform);
        }
        Ok(RelaxReport {
            written: 3,
            skipped: Vec::new(),
        })
    }

    fn ensure_marker(&self, dir: &ManagedDir) -> Result<MarkerOutcome, MarkerError> {
        let name = dir
            .path()
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        push(&self.trace, format!("acl:marker:{name}"));
        if let Some((key, error)) = self.marker.lock().unwrap().as_ref() {
            if *key == name {
                return Err(error.clone());
            }
        }
        Ok(MarkerOutcome::Existed)
    }

    fn recheck(&self, _dir: &ManagedDir, _expected: &DirAccess) -> Result<(), RecheckFailure> {
        let n = self.recheck_calls.fetch_add(1, Ordering::SeqCst) + 1;
        let fails_at = self.recheck_fails_at.load(Ordering::SeqCst);
        if fails_at != 0 && n >= fails_at {
            push(&self.trace, "acl:recheck-failed");
            return Err(RecheckFailure::NotADirectory);
        }
        Ok(())
    }

    fn watch(&self, _root: &SafeRoot) -> Result<Option<Box<dyn SecurityWatcher>>, String> {
        Ok(None)
    }
}

/// テスト用の本体: ルートは `layout`、記録は `install_user`、ログは本体の行を `trace` に集める
/// 偽物（`log-opened`・`flush` も記録する）。
fn test_program(
    layout: &DataLayout,
    install_user: Result<Option<String>, String>,
    trace: &Trace,
    acl: Arc<FakeAcl>,
    recheck_interval: Duration,
) -> ServerProgram {
    let layout = layout.clone();
    let trace_for_log = trace.clone();
    ServerProgram::new(ProgramParts {
        layout: Box::new(move || Ok(layout.clone())),
        read_install_user: Box::new(move || install_user.clone()),
        open_log: Box::new(move |_dir: Arc<ManagedDir>, _prunes: Arc<LogPrunes>| {
            push(&trace_for_log, "log-opened");
            let flush_trace = trace_for_log.clone();
            let sink_trace = trace_for_log.clone();
            Ok((
                None,
                Box::new(move |_| push(&flush_trace, "flush")),
                Some(Arc::new(move |line: &str| push(&sink_trace, line))),
            ))
        }),
        known_folders: Box::new(|| Ok(KnownFolders::default())),
        redirection_guard: Box::new(|| RedirectionGuard::NotWindows),
        acl,
        recheck_interval,
    })
}

fn program(layout: &DataLayout, trace: &Trace, acl: Arc<FakeAcl>) -> ServerProgram {
    test_program(
        layout,
        Ok(Some(USER.to_string())),
        trace,
        acl,
        Duration::from_secs(3600),
    )
}

// --- 偽の登録 ------------------------------------------------------------------------

type Seen = Arc<Mutex<Vec<(ServiceStatusPlan, ServiceExit)>>>;
/// 偽の登録が返す、報告を記録する関数。
type TestReport = Box<dyn Fn(&ServiceStatusPlan, ServiceExit) + Send + Sync>;

/// 偽の登録: ハンドラをテストへ渡し、報告を記録する（共有 crate の芯のテストと同じ形）。
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

fn has_phase(seen: &Seen, wanted: ServicePhase) -> bool {
    phases(seen).iter().any(|(phase, _)| *phase == wanted)
}

/// 動いているサービス（別スレッドの `run_program_with_args`）。
struct Running {
    handler: ControlHandler,
    seen: Seen,
    thread: Option<std::thread::JoinHandle<()>>,
    /// 本体の結果（スレッドが終わる直前に送る）。
    done: mpsc::Receiver<Option<ServiceExit>>,
}

impl Running {
    fn send(&self, control: ScmControlKind) {
        assert_eq!((self.handler)(control), ControlReply::NoError);
    }

    /// 本体が終わるのを [`JOIN_LIMIT`] まで待つ。超えたら STOP を送って止めてから、理由
    /// （そのときの報告の列）を出して落とす（ハングさせない）。
    fn join(mut self) -> Option<ServiceExit> {
        match self.done.recv_timeout(JOIN_LIMIT) {
            Ok(exit) => {
                let _ = self.thread.take().unwrap().join();
                exit
            }
            Err(_) => {
                let phases = phases(&self.seen);
                let _ = (self.handler)(ScmControlKind::Stop);
                let _ = self.done.recv_timeout(JOIN_LIMIT);
                let _ = self.thread.take().unwrap().join();
                panic!(
                    "本体が {JOIN_LIMIT:?} 以内に終わらなかった（{}）。報告の列: {phases:?}",
                    if phases.iter().any(|(p, _)| *p == ServicePhase::Running) {
                        "通常起動して Running のまま"
                    } else {
                        "Running を報告せずに止まったまま"
                    }
                );
            }
        }
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

fn spawn(program: ServerProgram) -> Running {
    spawn_with_args(program, &[])
}

/// `args` は `StartService` の引数（先頭のサービス名はここで足す）。
fn spawn_with_args(program: ServerProgram, args: &[&str]) -> Running {
    let (handler_tx, handler_rx) = mpsc::channel();
    let seen: Seen = Arc::new(Mutex::new(Vec::new()));
    let argv: Vec<OsString> = std::iter::once(SERVICE_NAME)
        .chain(args.iter().copied())
        .map(OsString::from)
        .collect();
    let (done_tx, done) = mpsc::channel();
    let thread = {
        let seen = seen.clone();
        std::thread::spawn(move || {
            let exit = run_program_with_args(program, argv, fake_register(handler_tx, seen));
            let _ = done_tx.send(exit);
        })
    };
    let handler = handler_rx
        .recv_timeout(Duration::from_secs(10))
        .expect("制御ハンドラが登録されない");
    Running {
        handler,
        seen,
        thread: Some(thread),
        done,
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
        has_phase(&running.seen, ServicePhase::Running).then_some(())
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
                settings: None,
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
    let body: serde_json::Value = serde_json::from_str(&response.text().await.unwrap()).unwrap();
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

/// 運用モードへの起動の順序（約束 1）: 1 周目 → 2 周目 → 印（`logs` を含む全部）→ エンジン
/// （Running）。締め終わる前に Running の行・DB の起動の行が無い。
fn assert_tightened_before_engine(lines: &[String]) {
    let scan = position(lines, "acl:scan");
    let close = position(lines, "acl:close");
    let running = position(lines, "Windows サービスとして起動しました");
    assert!(scan < close, "{lines:#?}");
    for key in [
        "logs",
        "run",
        "hub-keys",
        "data",
        "backups",
        "audit-spool",
        "secrets",
    ] {
        let marker = position(lines, &format!("acl:marker:{key}"));
        assert!(close < marker && marker < running, "{key}: {lines:#?}");
    }
    let begin = position(lines, "運用モードへの切り替えを開始");
    assert!(begin < scan, "{lines:#?}");
    let done = position(lines, "運用モードへの切り替えを完了");
    assert!(running < done, "{lines:#?}");
}

/// 設定モードへの停止の順序（約束 2）: 停止の完了の行 → 「緩め始めます」→ flush → 緩める。
/// **緩めた後に行が無い**。
fn assert_relaxed_last_and_silent(lines: &[String]) {
    let done = position(lines, "最終 flush を含む停止が完了しました");
    let begin = position(lines, "ACL を緩め始めます");
    let flush = lines
        .iter()
        .enumerate()
        .filter(|(_, line)| *line == "flush")
        .map(|(i, _)| i)
        .find(|i| *i > begin)
        .unwrap_or_else(|| panic!("緩め始めの後の flush が無い: {lines:#?}"));
    let relax = position(lines, "acl:relax");
    assert!(done < begin && begin < flush && flush < relax, "{lines:#?}");
    assert_eq!(relax, lines.len() - 1, "緩め始めた後に行がある: {lines:#?}");
}

/// `close_log` は**向け直しを流しきってから**日次ログを閉じる（Codex の指摘 P2）。閉じる前に
/// パイプに残っていた出力（`flush` のコールバックが `StdioRedirect::finish` で流す分）がログに
/// 残り、閉じた後の `write_line` は行を足さない。順を戻す（閉じてから流す）とこのテストが落ちる。
#[test]
fn close_log_drains_the_redirect_before_closing_the_log() {
    let dir = tempfile::tempdir().unwrap();
    let layout = DataLayout::new(dir.path().join("root"));
    let trace: Trace = Arc::default();
    let program = program(&layout, &trace, FakeAcl::new(trace.clone()));
    let log = Arc::new(Mutex::new(
        server_host::ServerLog::open(dir.path().join("logs"), Arc::new(banto_tstore::SystemClock))
            .unwrap(),
    ));
    let path = log.lock().unwrap().current_path();
    let for_flush = log.clone();
    program.attach_log(
        Box::new(move |_| {
            for_flush.lock().unwrap().write_line("残っていた出力");
        }),
        None,
    );
    program.close_log(Some(&log), Duration::from_secs(1));
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("残っていた出力"), "{text}");
    // 閉じた後は増えない。
    log.lock().unwrap().write_line("閉じた後の行");
    let after = std::fs::read_to_string(&path).unwrap();
    assert_eq!(after, text);
}

// --- 判定表 ----------------------------------------------------------------------------

/// 運用モードへ → 収集 → SCM の STOP = 設定モードへ。Running の報告は 1 回、締め終わって
/// から Running、STOP で最終 flush → 緩める → Stopped(0)。host の種類は `service`
/// （`endpoint.json` と `GET /api/host`）、コンソールの Ctrl のハンドラーは登録しない。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn runs_as_a_service_tightens_first_and_relaxes_last_on_scm_stop() {
    let dir = tempfile::tempdir().unwrap();
    let layout = DataLayout::new(dir.path().join("root"));
    let sim = simulation::start(Protocol::ModbusTcp).await;
    prepare_root_db(&layout, Some(sim.addr().port())).await;
    let trace: Trace = Arc::default();
    let acl = FakeAcl::new(trace.clone());
    let running = spawn(program(&layout, &trace, acl));
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
    // 管理するフォルダは運用モードへ切り替えた後に作られている。
    for path in [
        layout.logs_dir(),
        layout.run_dir(),
        layout.hub_keys_dir(),
        layout.backups_dir(),
        layout.audit_spool_dir(),
    ] {
        assert!(path.is_dir(), "{} が無い", path.display());
    }
    assert_tightened_before_engine(&snapshot(&trace));

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
    assert!(has_phase(&seen, ServicePhase::StartPending), "{phases:?}");
    assert!(has_phase(&seen, ServicePhase::StopPending), "{phases:?}");
    assert_eq!(
        phases.last(),
        Some(&(ServicePhase::Stopped, ServiceExit::Success))
    );
    drop(sim);

    let lines = snapshot(&trace);
    assert_relaxed_last_and_silent(&lines);
    assert_eq!(
        acl_calls(&lines).last().map(String::as_str),
        Some("acl:relax")
    );
    assert!(!layout.endpoint_file().exists(), "endpoint.json が残った");
    assert_eq!(
        latest_event(&layout).await.as_deref(),
        Some("collection_stopped")
    );
    assert!(stored_row_count(&layout).await > 0, "行が残っていない");
}

/// Codex の指摘 P1（#623 PR-3）: STOP の後始末が予算内に終わらなかった（`timed_out`。DB の
/// pool や listener のタスクが残りうる）なら、**緩めずに**締めたまま Stopped(5)。「緩め始めます」
/// の行も `acl:relax` も出ない（反証は上の `runs_as_a_service_tightens_first_and_relaxes_last_on_scm_stop`:
/// 時間切れでなければ緩めて 0）。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_timed_out_shutdown_on_scm_stop_keeps_the_tree_tight_with_5() {
    let dir = tempfile::tempdir().unwrap();
    let layout = DataLayout::new(dir.path().join("root"));
    prepare_root_db(&layout, None).await;
    let trace: Trace = Arc::default();
    let acl = FakeAcl::new(trace.clone());
    let program = program(&layout, &trace, acl);
    program.fake_stop_timed_out.store(true, Ordering::SeqCst);
    let running = spawn(program);
    wait_running(&running).await;
    assert!(layout.endpoint_file().exists());

    running.send(ScmControlKind::Stop);
    let seen = running.seen.clone();
    assert_eq!(running.join(), Some(ServiceExit::Specific(5)));
    let phases = phases(&seen);
    assert!(has_phase(&seen, ServicePhase::StopPending), "{phases:?}");
    assert_eq!(
        phases.last(),
        Some(&(ServicePhase::Stopped, ServiceExit::Specific(5)))
    );

    let lines = snapshot(&trace);
    assert!(
        index_of(&lines, "ACL を緩めずに終わります").is_some(),
        "{lines:#?}"
    );
    assert!(
        index_of(&lines, "ACL を緩め始めます").is_none(),
        "時間切れなのに緩め始めた: {lines:#?}"
    );
    assert!(
        index_of(&lines, "acl:relax").is_none(),
        "時間切れなのに緩めた: {lines:#?}"
    );
    // エンジンは止めている（endpoint.json は消えている）。残るのは ACL の形だけ。
    assert!(!layout.endpoint_file().exists(), "エンジンを止めていない");
}

/// オーナーレビュー P1（#623 PR-3）の再現: STOP の前に `POST /api/auth/login` のヘッダと本文の
/// 一部だけ送って保留した接続は、banto の listener の停止（5 秒で親を abort）では止まらない。
/// サービスは緩める前にエンジンのランタイムを終わらせるので、その接続のタスクも捨てられる。
///
/// 退行を直接捉えるため（PR #683 のオーナーの提案）、偽の `relax_tree` の**中で**本体を待たせ、
/// **緩めている最中に**本文を完了させる。要求は処理されず、監査の退避ファイル（`audit-spool\`）
/// も利用者の木の下に書かれない。Stopped の後に本文を送る形では、ふつうのランタイムの破棄
/// （本体の終わり）でも接続が止まるので、「緩める前にランタイムを終わらせる」を省いた退行を
/// 見分けられなかった（反証: `shutdown_within` の `drop(runtime)` を `mem::forget` にすると、
/// 本文の完了で要求が処理されて落ちる）。緩め始めた後はプロセスの標準出力・標準エラー出力が
/// NUL に向くので（`close_log`）、ここで落ちたときは panic の文言が出ず終了コード 101 だけになる。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_request_held_open_across_scm_stop_cannot_write_after_relaxing() {
    use std::io::{Read, Write};

    let dir = tempfile::tempdir().unwrap();
    let layout = DataLayout::new(dir.path().join("root"));
    prepare_root_db(&layout, None).await;
    let trace: Trace = Arc::default();
    let acl = FakeAcl::new(trace.clone());
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    *acl.relax_hold.lock().unwrap() = Some((entered_tx, release_rx));
    let running = spawn(program(&layout, &trace, acl));
    // `running` より後に束ね直す: 途中で落ちたら先に drop され、偽の `relax_tree` の待ちが
    // すぐ解ける（`Running` の drop が [`RELAX_HOLD_LIMIT`] まで待たない）。
    let release_tx = release_tx;
    wait_running(&running).await;
    let endpoint = read_endpoint(&layout.endpoint_file()).expect("endpoint.json");
    let _token = setup_admin(endpoint.port).await;
    let spool_before = spool_files(&layout);

    // ヘッダと本文の一部だけ送って保留する（本物の TCP。keep-alive のまま）。
    let body = serde_json::json!({"username":"admin","password":"wrong-pass-1"}).to_string();
    let (head, tail) = body.split_at(10);
    let mut stream = std::net::TcpStream::connect(("127.0.0.1", endpoint.port)).expect("connect");
    stream
        .write_all(
            format!(
                "POST /api/auth/login HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nX-Banto-Client: banto\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{head}",
                endpoint.port,
                body.len()
            )
            .as_bytes(),
        )
        .expect("write head");
    stream.flush().unwrap();

    // STOP。ランタイムを終わらせてから緩め始め、偽の `relax_tree` の中で止まる。
    let stop_sent = Instant::now();
    running.send(ScmControlKind::Stop);
    entered_rx
        .recv_timeout(JOIN_LIMIT)
        .expect("緩めるところまで来なかった");
    let lines = snapshot(&trace);
    let finished = index_of(&lines, "非同期ランタイムを終わらせました");
    let begin = index_of(&lines, "ACL を緩め始めます");
    let relax = index_of(&lines, "acl:relax");
    assert!(
        finished.is_some() && begin.is_some() && relax.is_some(),
        "{lines:#?}"
    );
    assert!(finished < begin && begin < relax, "{lines:#?}");

    // 緩めている最中に、banto の graceful 停止（5 秒）も超えてから本文を完了させる。
    if let Some(rest) = Duration::from_secs(6).checked_sub(stop_sent.elapsed()) {
        std::thread::sleep(rest);
    }
    stream
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    let written = stream
        .write_all(tail.as_bytes())
        .and_then(|()| stream.flush());
    let mut response = Vec::new();
    let read = stream.read_to_end(&mut response);
    let text = String::from_utf8_lossy(&response);
    assert!(
        !text.starts_with("HTTP/1.1"),
        "緩めている最中に要求が処理された（write={written:?}, read={read:?}）: {text}"
    );
    // 退避ファイルは増えていない（しばらく見張る - 退避の書き込みは非同期）。
    std::thread::sleep(Duration::from_secs(1));
    assert_eq!(
        spool_files(&layout),
        spool_before,
        "緩めている最中に監査の退避が書かれた"
    );

    // 放して、0 で終わる。
    release_tx.send(()).unwrap();
    let seen = running.seen.clone();
    assert_eq!(running.join(), Some(ServiceExit::Success));
    assert_eq!(
        phases(&seen).last(),
        Some(&(ServicePhase::Stopped, ServiceExit::Success))
    );
    assert!(!layout.endpoint_file().exists());
}

/// `audit-spool\` の下のファイル名（無ければ空）。
fn spool_files(layout: &DataLayout) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(layout.audit_spool_dir())
        .map(|entries| {
            entries
                .flatten()
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

/// #623 PR-3 の後追い: 古いログの削除のスレッド（`logs\` の保持期間の掃除）が STOP の時点で
/// 走っていたら、終わるのを待ってから緩める（削除は握った `logs\` の子を開いて消すので、緩めと
/// 重ねない）。偽の削除は STOP を受けた後で放され、終わりを列に記録する（反証: 待ちを省くと
/// 削除の途中で `acl:relax` が来て落ちる）。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_running_log_prune_finishes_before_relaxing() {
    let dir = tempfile::tempdir().unwrap();
    let layout = DataLayout::new(dir.path().join("root"));
    prepare_root_db(&layout, None).await;
    let trace: Trace = Arc::default();
    let acl = FakeAcl::new(trace.clone());
    let program = program(&layout, &trace, acl);
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let prune_trace = trace.clone();
    *program.fake_log_prune.lock().unwrap() = Some(Box::new(move || {
        push(&prune_trace, "prune:started");
        let _ = release_rx.recv_timeout(JOIN_LIMIT);
        push(&prune_trace, "prune:done");
    }));
    let running = spawn(program);
    wait_running(&running).await;
    wait_for("削除の開始", async || {
        index_of(&snapshot(&trace), "prune:started").map(|_| ())
    })
    .await;

    running.send(ScmControlKind::Stop);
    // STOP の後も削除が続いている間は緩めない（後始末とランタイムの終わりを越えるまで見る）。
    wait_for("ランタイムの終わり", async || {
        index_of(&snapshot(&trace), "非同期ランタイムを終わらせました").map(|_| ())
    })
    .await;
    std::thread::sleep(Duration::from_millis(500));
    let lines = snapshot(&trace);
    assert!(
        index_of(&lines, "acl:relax").is_none(),
        "削除の途中で緩めた: {lines:#?}"
    );
    release_tx.send(()).unwrap();
    let seen = running.seen.clone();
    assert_eq!(running.join(), Some(ServiceExit::Success));
    assert_eq!(
        phases(&seen).last(),
        Some(&(ServicePhase::Stopped, ServiceExit::Success))
    );
    let lines = snapshot(&trace);
    let done = position(&lines, "prune:done");
    let begin = position(&lines, "ACL を緩め始めます");
    let relax = position(&lines, "acl:relax");
    assert!(done < begin && begin < relax, "{lines:#?}");
}

/// #623 PR-3 の後追い: 古いログの削除が上限（テストは 1 秒）の中で終わらなければ、**緩めずに**
/// 締めたまま Stopped(5)。「緩め始めます」も `acl:relax` も出ない（反証: 待ちを省くと 0 で
/// 緩めて落ちる）。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_log_prune_that_does_not_end_within_the_wait_keeps_the_tree_tight_with_5() {
    let dir = tempfile::tempdir().unwrap();
    let layout = DataLayout::new(dir.path().join("root"));
    prepare_root_db(&layout, None).await;
    let trace: Trace = Arc::default();
    let acl = FakeAcl::new(trace.clone());
    let mut program = program(&layout, &trace, acl);
    program.log_prune_wait = Duration::from_secs(1);
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let prune_trace = trace.clone();
    *program.fake_log_prune.lock().unwrap() = Some(Box::new(move || {
        push(&prune_trace, "prune:started");
        let _ = release_rx.recv_timeout(JOIN_LIMIT);
    }));
    let running = spawn(program);
    wait_running(&running).await;
    wait_for("削除の開始", async || {
        index_of(&snapshot(&trace), "prune:started").map(|_| ())
    })
    .await;

    running.send(ScmControlKind::Stop);
    let seen = running.seen.clone();
    let exit = running.join();
    let _ = release_tx.send(());
    assert_eq!(exit, Some(ServiceExit::Specific(5)));
    assert_eq!(
        phases(&seen).last(),
        Some(&(ServicePhase::Stopped, ServiceExit::Specific(5)))
    );
    let lines = snapshot(&trace);
    assert!(
        index_of(&lines, "古いログの削除が1秒待っても終わらなかった").is_some(),
        "{lines:#?}"
    );
    assert!(
        index_of(&lines, "ACL を緩め始めます").is_none(),
        "{lines:#?}"
    );
    assert!(index_of(&lines, "acl:relax").is_none(), "{lines:#?}");
    // エンジンは止めている（endpoint.json は消えている）。残るのは ACL の形だけ。
    assert!(!layout.endpoint_file().exists(), "エンジンを止めていない");
}

/// 本物の日次ログ（`ServerLog::open_in` + `StdioRedirect` の向け直し）で動く本体。時計は
/// `clock`（テストが日付を進めて、向け直しの書き込みに日付の切り替えを起こさせる）。
fn real_log_program(
    layout: &DataLayout,
    trace: &Trace,
    acl: Arc<FakeAcl>,
    clock: Arc<banto_tstore::ManualClock>,
) -> ServerProgram {
    let mut program = program(layout, trace, acl);
    program.open_log = Box::new(move |dir: Arc<ManagedDir>, prunes: Arc<LogPrunes>| {
        crate::open_log_in_with_clock(dir, prunes, clock.clone())
    });
    program
}

/// 日付の切り替えで起きる削除を保留する（`LogPrunes` の各削除の直前で `release` を待つ）。
/// 起動時の `set_retention` の分が終わってから差し込む（それは保留しない）。
async fn hold_rollover_prunes(prunes: &Arc<LogPrunes>, trace: &Trace) -> mpsc::Sender<()> {
    wait_for(
        "起動時の古いログの削除の終わり",
        async || (prunes.running() == 0).then_some(()),
    )
    .await;
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let release_rx = Arc::new(Mutex::new(release_rx));
    let hook_trace = trace.clone();
    prunes.set_before_run(Some(Arc::new(move || {
        push(&hook_trace, "prune:started");
        // 送り口が落ちる（テストが放す）か上限で戻る。
        let _ = release_rx.lock().unwrap().recv_timeout(JOIN_LIMIT);
        push(&hook_trace, "prune:released");
    })));
    release_tx
}

/// 今の（`clock` の）日付の日次ログの中身（無ければ空）。
fn day_log(layout: &DataLayout, clock: &banto_tstore::ManualClock) -> String {
    use banto_tstore::Clock as _;
    let date = banto_tstore::LocalDate::from_epoch_ms(clock.now_ms(), clock.utc_offset_ms());
    std::fs::read_to_string(layout.logs_dir().join(server_host::log_file_name(date)))
        .unwrap_or_default()
}

fn manual_clock_now() -> Arc<banto_tstore::ManualClock> {
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;
    Arc::new(banto_tstore::ManualClock::new(now_ms, 0))
}

/// Codex の P1・オーナーレビューの P2（#623 PR-3 の後追い）: サービスが日付をまたいだ後の STOP で、
/// 停止の行（本物の `StdioRedirect::write_through` → `ServerLog::write_line`）が日付の切り替えを
/// 起こして始めた古いログの削除も、終わるまで緩めない。削除は保留しておき、ランタイムの終わりを
/// 越えても `acl:relax` が来ないことを見てから放す（反証: 向け直しの削除を追跡する口に通さず
/// 切り離すと、保留中に緩めて落ちる）。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_log_prune_started_by_a_date_rollover_finishes_before_relaxing() {
    let dir = tempfile::tempdir().unwrap();
    let layout = DataLayout::new(dir.path().join("root"));
    prepare_root_db(&layout, None).await;
    let trace: Trace = Arc::default();
    let acl = FakeAcl::new(trace.clone());
    let clock = manual_clock_now();
    let program = real_log_program(&layout, &trace, acl, clock.clone());
    let prunes = Arc::clone(&program.log_prunes);
    let running = spawn(program);
    wait_running(&running).await;
    let release = hold_rollover_prunes(&prunes, &trace).await;

    // 日付をまたぐ。STOP の最初の行が新しい日のファイルを開き、削除を返す。
    clock.advance_ms(86_400_000);
    running.send(ScmControlKind::Stop);
    wait_for("日付の切り替えの削除の開始", async || {
        index_of(&snapshot(&trace), "prune:started").map(|_| ())
    })
    .await;
    wait_for("ランタイムの終わり", async || {
        day_log(&layout, &clock)
            .contains("非同期ランタイムを終わらせました")
            .then_some(())
    })
    .await;
    std::thread::sleep(Duration::from_millis(500));
    let lines = snapshot(&trace);
    assert!(
        index_of(&lines, "acl:relax").is_none(),
        "日付の切り替えの削除の途中で緩めた: {lines:#?}"
    );
    drop(release);
    let seen = running.seen.clone();
    assert_eq!(running.join(), Some(ServiceExit::Success));
    assert_eq!(
        phases(&seen).last(),
        Some(&(ServicePhase::Stopped, ServiceExit::Success))
    );
    let lines = snapshot(&trace);
    assert!(
        position(&lines, "prune:released") < position(&lines, "acl:relax"),
        "{lines:#?}"
    );
    assert!(day_log(&layout, &clock).contains("ACL を緩め始めます"));
}

/// 同上で、日付の切り替えの削除が上限（テストは 1 秒）の中で終わらなければ、**緩めずに**締めた
/// まま Stopped(5)（反証: 追跡を外すと 0 で緩めて落ちる）。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_rollover_log_prune_that_does_not_end_within_the_wait_keeps_the_tree_tight_with_5() {
    let dir = tempfile::tempdir().unwrap();
    let layout = DataLayout::new(dir.path().join("root"));
    prepare_root_db(&layout, None).await;
    let trace: Trace = Arc::default();
    let acl = FakeAcl::new(trace.clone());
    let clock = manual_clock_now();
    let mut program = real_log_program(&layout, &trace, acl, clock.clone());
    program.log_prune_wait = Duration::from_secs(1);
    let prunes = Arc::clone(&program.log_prunes);
    let running = spawn(program);
    wait_running(&running).await;
    let release = hold_rollover_prunes(&prunes, &trace).await;

    clock.advance_ms(86_400_000);
    running.send(ScmControlKind::Stop);
    let seen = running.seen.clone();
    let exit = running.join();
    drop(release);
    assert_eq!(exit, Some(ServiceExit::Specific(5)));
    assert_eq!(
        phases(&seen).last(),
        Some(&(ServicePhase::Stopped, ServiceExit::Specific(5)))
    );
    let lines = snapshot(&trace);
    assert!(index_of(&lines, "prune:started").is_some(), "{lines:#?}");
    assert!(index_of(&lines, "acl:relax").is_none(), "{lines:#?}");
    let text = day_log(&layout, &clock);
    assert!(
        text.contains("古いログの削除が1秒待っても終わらなかった"),
        "{text}"
    );
    assert!(!text.contains("ACL を緩め始めます"), "{text}");
}

/// オーナーレビュー P1（#623 PR-3）: エンジンのランタイムを上限の中で終わらせられなかった
/// （ブロッキングの処理が残っている）なら、**緩めずに**締めたまま Stopped(5)。「緩め始めます」
/// も `acl:relax` も出ない。本物のランタイムに、上限（1 秒）より長く眠る `spawn_blocking` を置く。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_runtime_that_does_not_end_within_the_wait_keeps_the_tree_tight_with_5() {
    let dir = tempfile::tempdir().unwrap();
    let layout = DataLayout::new(dir.path().join("root"));
    prepare_root_db(&layout, None).await;
    let trace: Trace = Arc::default();
    let acl = FakeAcl::new(trace.clone());
    let mut program = program(&layout, &trace, acl);
    program.runtime_shutdown_wait = Duration::from_secs(1);
    *program.fake_blocking_hold.lock().unwrap() = Some(Duration::from_secs(4));
    let running = spawn(program);
    wait_running(&running).await;
    assert!(layout.endpoint_file().exists());

    running.send(ScmControlKind::Stop);
    let seen = running.seen.clone();
    assert_eq!(running.join(), Some(ServiceExit::Specific(5)));
    let phases = phases(&seen);
    assert!(has_phase(&seen, ServicePhase::StopPending), "{phases:?}");
    assert_eq!(
        phases.last(),
        Some(&(ServicePhase::Stopped, ServiceExit::Specific(5)))
    );

    let lines = snapshot(&trace);
    assert!(
        index_of(&lines, "非同期ランタイムが1秒待っても終わらなかった").is_some()
            && index_of(&lines, "ACL を緩めずに終わります").is_some(),
        "{lines:#?}"
    );
    assert!(
        index_of(&lines, "非同期ランタイムを終わらせました").is_none(),
        "{lines:#?}"
    );
    assert!(
        index_of(&lines, "ACL を緩め始めます").is_none(),
        "ランタイムが残りうるのに緩め始めた: {lines:#?}"
    );
    assert!(
        index_of(&lines, "acl:relax").is_none(),
        "ランタイムが残りうるのに緩めた: {lines:#?}"
    );
    assert!(!layout.endpoint_file().exists(), "エンジンを止めていない");
    // 置いたブロッキングのタスクが終わるのを待ってから抜ける（後のテストに重ねない）。
    std::thread::sleep(Duration::from_secs(4));
}

/// 反証: ブロッキングの処理が上限の中で終われば、終わらせた行を出してから緩めて 0。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_runtime_that_ends_within_the_wait_relaxes_and_exits_0() {
    let dir = tempfile::tempdir().unwrap();
    let layout = DataLayout::new(dir.path().join("root"));
    prepare_root_db(&layout, None).await;
    let trace: Trace = Arc::default();
    let acl = FakeAcl::new(trace.clone());
    let mut program = program(&layout, &trace, acl);
    program.runtime_shutdown_wait = Duration::from_secs(5);
    *program.fake_blocking_hold.lock().unwrap() = Some(Duration::from_millis(500));
    let running = spawn(program);
    wait_running(&running).await;

    running.send(ScmControlKind::Stop);
    let seen = running.seen.clone();
    assert_eq!(running.join(), Some(ServiceExit::Success));
    assert_eq!(
        phases(&seen).last(),
        Some(&(ServicePhase::Stopped, ServiceExit::Success))
    );
    let lines = snapshot(&trace);
    let finished = index_of(&lines, "非同期ランタイムを終わらせました");
    let begin = index_of(&lines, "ACL を緩め始めます");
    let relax = index_of(&lines, "acl:relax");
    assert!(
        finished.is_some() && begin.is_some() && relax.is_some(),
        "{lines:#?}"
    );
    assert!(finished < begin && begin < relax, "{lines:#?}");
    assert!(
        index_of(&lines, "ACL を緩めずに終わります").is_none(),
        "{lines:#?}"
    );
    assert!(!layout.endpoint_file().exists());
}

/// A1 の排他を別のプロセス（ここではデスクトップの印）が持っていれば、ログを開かず・
/// ルートの下に何も作らず・ACL に触れずに Stopped(2)。Running は報告しない。
#[test]
fn a_held_instance_lock_stops_with_2_without_creating_anything() {
    let dir = tempfile::tempdir().unwrap();
    let layout = DataLayout::new(dir.path());
    let _desktop = chronogazer_core::instance_lock::acquire_for_db(
        &layout.db_path(),
        ChronoGazerHost::Desktop,
    )
    .unwrap();
    let trace: Trace = Arc::default();
    let running = spawn(program(&layout, &trace, FakeAcl::new(trace.clone())));
    let seen = running.seen.clone();
    assert_eq!(running.join(), Some(ServiceExit::Specific(2)));
    assert_eq!(
        phases(&seen),
        vec![(ServicePhase::Stopped, ServiceExit::Specific(2))]
    );
    let lines = snapshot(&trace);
    assert!(
        index_of(&lines, "log-opened").is_none(),
        "ログを開いた: {lines:?}"
    );
    assert!(acl_calls(&lines).is_empty(), "{lines:?}");
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

/// PR #618 の Codex の指摘: 日次ログの置き場を開けない（`logs` がファイル）ときは、1 周目の前に
/// 黙って続けず、Running を報告せずに Stopped(3)。ACL には触れない。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unopenable_log_dir_stops_with_3_before_touching_the_tree() {
    let dir = tempfile::tempdir().unwrap();
    let layout = DataLayout::new(dir.path().join("root"));
    prepare_root_db(&layout, None).await;
    std::fs::write(layout.logs_dir(), b"not a directory").unwrap();
    let trace: Trace = Arc::default();
    let running = spawn(program(&layout, &trace, FakeAcl::new(trace.clone())));
    let seen = running.seen.clone();
    assert_eq!(running.join(), Some(ServiceExit::Specific(3)));
    assert!(
        !has_phase(&seen, ServicePhase::Running),
        "{:?}",
        phases(&seen)
    );
    assert_eq!(
        phases(&seen).last(),
        Some(&(ServicePhase::Stopped, ServiceExit::Specific(3)))
    );
    let lines = snapshot(&trace);
    assert!(acl_calls(&lines).is_empty(), "{lines:?}");
    assert!(!layout.endpoint_file().exists(), "endpoint.json を書いた");
}

/// OS の PRESHUTDOWN: OS の期限（受けてから 27 秒）より前に Stopped(0) まで終わり、
/// **緩めない**（締めたまま。運用モードは再起動をまたいで続く）。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn preshutdown_finishes_within_the_os_budget_and_keeps_the_tree_tight() {
    let dir = tempfile::tempdir().unwrap();
    let layout = DataLayout::new(dir.path().join("root"));
    prepare_root_db(&layout, None).await;
    let trace: Trace = Arc::default();
    let running = spawn(program(&layout, &trace, FakeAcl::new(trace.clone())));
    wait_running(&running).await;
    let received = Instant::now();
    running.send(ScmControlKind::Preshutdown);
    let seen = running.seen.clone();
    assert_eq!(running.join(), Some(ServiceExit::Success));
    let elapsed = received.elapsed();
    assert!(
        elapsed
            < banto_service_control::stop_work_budget(banto_service_control::PRESHUTDOWN_TIMEOUT),
        "{elapsed:?}"
    );
    assert_eq!(
        phases(&seen).last(),
        Some(&(ServicePhase::Stopped, ServiceExit::Success))
    );
    assert!(!layout.endpoint_file().exists());
    let lines = snapshot(&trace);
    assert!(
        index_of(&lines, "acl:relax").is_none(),
        "PRESHUTDOWN で緩めた: {lines:#?}"
    );
    assert!(index_of(&lines, "Windows サービスを停止しました").is_some());
}

/// D1 の記録が無い（`NoReader`）: ログを開いて理由を書き、**何も変えずに** Stopped(4)。
/// 1 周目にも入らず、管理するフォルダも作らない。ルートも無ければ、何も作らずに 4。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_missing_install_user_record_refuses_with_4_without_changing_anything() {
    let dir = tempfile::tempdir().unwrap();
    let layout = DataLayout::new(dir.path().join("root"));
    prepare_root_db(&layout, None).await;
    let trace: Trace = Arc::default();
    let running = spawn(test_program(
        &layout,
        Ok(None),
        &trace,
        FakeAcl::new(trace.clone()),
        Duration::from_secs(3600),
    ));
    let seen = running.seen.clone();
    assert_eq!(running.join(), Some(ServiceExit::Specific(4)));
    assert!(
        !has_phase(&seen, ServicePhase::Running),
        "{:?}",
        phases(&seen)
    );
    let lines = snapshot(&trace);
    assert!(index_of(&lines, "log-opened").is_some(), "{lines:?}");
    assert!(
        index_of(&lines, "インストール利用者の記録").is_some(),
        "{lines:?}"
    );
    assert!(acl_calls(&lines).is_empty(), "{lines:?}");
    assert!(!layout.secrets_dir().exists(), "管理するフォルダを作った");
    assert!(!layout.run_dir().exists());

    // ルートが無い: 何も作らずに 4（ログも無い）。
    let absent = DataLayout::new(dir.path().join("absent"));
    let trace: Trace = Arc::default();
    let running = spawn(test_program(
        &absent,
        Ok(None),
        &trace,
        FakeAcl::new(trace.clone()),
        Duration::from_secs(3600),
    ));
    assert_eq!(running.join(), Some(ServiceExit::Specific(4)));
    assert!(
        !absent.root().exists(),
        "相手が決まらないのにルートを作った"
    );
    assert!(snapshot(&trace).is_empty());
}

/// 起動の途中（ログを開いたところ）に STOP を受けたら、1 周目が合図を見て何も変えずに中止
/// （Stopped(0)、失敗ではない）。Running は報告せず、締めも緩めもしない。
#[test]
fn a_stop_during_the_start_aborts_before_tightening_as_success() {
    let dir = tempfile::tempdir().unwrap();
    let layout = DataLayout::new(dir.path().join("root"));
    let (opened_tx, opened_rx) = mpsc::channel::<()>();
    let (go_tx, go_rx) = mpsc::channel::<()>();
    let go_rx = Mutex::new(go_rx);
    let trace: Trace = Arc::default();
    let acl = FakeAcl::new(trace.clone());
    let program = ServerProgram::new(ProgramParts {
        layout: Box::new({
            let layout = layout.clone();
            move || Ok(layout.clone())
        }),
        read_install_user: Box::new(|| Ok(Some(USER.to_string()))),
        // ログを開いたところで止まり、テストが STOP を送ってから進む。
        open_log: Box::new(move |_dir: Arc<ManagedDir>, _prunes: Arc<LogPrunes>| {
            let _ = opened_tx.send(());
            let _ = go_rx.lock().unwrap().recv_timeout(Duration::from_secs(20));
            Ok((None, Box::new(|_| {}), None))
        }),
        known_folders: Box::new(|| Ok(KnownFolders::default())),
        redirection_guard: Box::new(|| RedirectionGuard::NotWindows),
        acl,
        recheck_interval: Duration::from_secs(3600),
    });
    let running = spawn(program);
    opened_rx
        .recv_timeout(Duration::from_secs(20))
        .expect("ログを開くところまで来ない");
    running.send(ScmControlKind::Stop);
    go_tx.send(()).unwrap();
    let seen = running.seen.clone();
    assert_eq!(running.join(), Some(ServiceExit::Success));
    let phases = phases(&seen);
    assert!(!has_phase(&seen, ServicePhase::Running), "{phases:?}");
    assert_eq!(
        phases.last(),
        Some(&(ServicePhase::Stopped, ServiceExit::Success))
    );
    assert_eq!(acl_calls(&snapshot(&trace)), ["acl:scan"]);
    assert!(!layout.db_path().exists(), "DB を開いた");
}

/// 開始の引数に知らない語（`relax-only` の綴り違い・余計な語）: **何も変えずに** Stopped(3)。
/// ルートも作らず、ログも開かず、ACL にも触れない。Running は報告しない。
#[test]
fn an_unknown_start_arg_changes_nothing_and_stops_with_3() {
    for args in [
        &["relax_only"][..],
        &["Relax-Only"],
        &["relax-only", "x"],
        &["--help"],
    ] {
        let dir = tempfile::tempdir().unwrap();
        let layout = DataLayout::new(dir.path().join("root"));
        let trace: Trace = Arc::default();
        let running = spawn_with_args(program(&layout, &trace, FakeAcl::new(trace.clone())), args);
        let seen = running.seen.clone();
        assert_eq!(running.join(), Some(ServiceExit::Specific(3)), "{args:?}");
        assert_eq!(
            phases(&seen),
            vec![(ServicePhase::Stopped, ServiceExit::Specific(3))],
            "{args:?}"
        );
        assert!(!layout.root().exists(), "{args:?}: ルートを作った");
        assert!(snapshot(&trace).is_empty(), "{args:?}");
    }
}

/// `relax-only`: 緩めるだけ。エンジンを動かさず（DB を開かない・`endpoint.json` を書かない）、
/// **Running を報告せずに** Stopped(0)。`logs` が無ければ作らずログなし、あればログに
/// 「relax-only で開始」が最後の行で、緩めた後は書かない。ルートが無ければ何もせず 0。
/// 相手を決められなければ緩めずに 5。
#[test]
fn relax_only_relaxes_without_running_the_engine() {
    // ルートはあるが logs は無い。
    let dir = tempfile::tempdir().unwrap();
    let layout = DataLayout::new(dir.path().join("root"));
    std::fs::create_dir_all(layout.root()).unwrap();
    let trace: Trace = Arc::default();
    let running = spawn_with_args(
        program(&layout, &trace, FakeAcl::new(trace.clone())),
        &["relax-only"],
    );
    let seen = running.seen.clone();
    assert_eq!(running.join(), Some(ServiceExit::Success));
    assert!(
        !has_phase(&seen, ServicePhase::Running),
        "{:?}",
        phases(&seen)
    );
    assert!(
        has_phase(&seen, ServicePhase::StartPending),
        "{:?}",
        phases(&seen)
    );
    assert_eq!(
        phases(&seen).last(),
        Some(&(ServicePhase::Stopped, ServiceExit::Success))
    );
    assert_eq!(acl_calls(&snapshot(&trace)), ["acl:relax"]);
    assert!(index_of(&snapshot(&trace), "log-opened").is_none());
    assert!(!layout.logs_dir().exists(), "relax-only が logs を作った");
    assert!(!layout.db_path().exists(), "relax-only が DB を開いた");
    assert!(!layout.endpoint_file().exists());

    // logs がある: ログに書き、「relax-only で開始」の後は flush → 緩める → 何も無い。
    std::fs::create_dir_all(layout.logs_dir()).unwrap();
    let trace: Trace = Arc::default();
    let running = spawn_with_args(
        program(&layout, &trace, FakeAcl::new(trace.clone())),
        &["relax-only"],
    );
    assert_eq!(running.join(), Some(ServiceExit::Success));
    let lines = snapshot(&trace);
    let begin = position(&lines, "relax-only で開始しました");
    let flush = position(&lines, "flush");
    let relax = position(&lines, "acl:relax");
    assert!(begin < flush && flush < relax, "{lines:#?}");
    assert_eq!(relax, lines.len() - 1, "{lines:#?}");

    // ルートが無い: 何もせず 0。
    let absent = DataLayout::new(dir.path().join("absent"));
    let trace: Trace = Arc::default();
    let running = spawn_with_args(
        program(&absent, &trace, FakeAcl::new(trace.clone())),
        &["relax-only"],
    );
    assert_eq!(running.join(), Some(ServiceExit::Success));
    assert!(!absent.root().exists());
    assert!(snapshot(&trace).is_empty());

    // 相手を決められない: 緩めずに 5。
    let trace: Trace = Arc::default();
    let running = spawn_with_args(
        test_program(
            &layout,
            Ok(None),
            &trace,
            FakeAcl::new(trace.clone()),
            Duration::from_secs(3600),
        ),
        &["relax-only"],
    );
    assert_eq!(running.join(), Some(ServiceExit::Specific(5)));
    let lines = snapshot(&trace);
    assert!(acl_calls(&lines).is_empty(), "{lines:?}");
    assert!(
        index_of(&lines, "緩めずに終わります").is_some(),
        "{lines:?}"
    );

    // 緩めるのに失敗: 5。
    let trace: Trace = Arc::default();
    let acl = FakeAcl::new(trace.clone());
    acl.relax_fails.store(true, Ordering::SeqCst);
    let running = spawn_with_args(program(&layout, &trace, acl), &["relax-only"]);
    assert_eq!(running.join(), Some(ServiceExit::Specific(5)));
}

/// 1 周目が断った: 一覧をログに書き、**何も変えずに** Stopped(4)。2 周目にも管理する
/// フォルダにもエンジンにも進まない。I/O の失敗なら 3。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_scan_refusal_stops_with_4_without_closing_the_tree() {
    let dir = tempfile::tempdir().unwrap();
    let layout = DataLayout::new(dir.path().join("root"));
    prepare_root_db(&layout, None).await;
    let trace: Trace = Arc::default();
    let acl = FakeAcl::new(trace.clone());
    *acl.scan.lock().unwrap() = Some(ScanRefusal::Problems(vec![
        TreeProblem::Hardlink {
            path: layout.data_dir().join("x.sqlite3"),
            links: 2,
        },
        TreeProblem::ReparsePoint {
            path: layout.logs_dir(),
            tag: 0xA000_0003,
        },
    ]));
    let running = spawn(program(&layout, &trace, acl));
    let seen = running.seen.clone();
    assert_eq!(running.join(), Some(ServiceExit::Specific(4)));
    assert!(!has_phase(&seen, ServicePhase::Running));
    let lines = snapshot(&trace);
    assert_eq!(acl_calls(&lines), ["acl:scan"]);
    assert!(
        index_of(&lines, "締められない物が 2 件").is_some(),
        "{lines:#?}"
    );
    assert!(index_of(&lines, "ハードリンク").is_some(), "{lines:#?}");
    assert!(index_of(&lines, "reparse point").is_some(), "{lines:#?}");
    assert!(!layout.secrets_dir().exists(), "管理するフォルダを作った");
    assert!(!layout.endpoint_file().exists());

    let trace: Trace = Arc::default();
    let acl = FakeAcl::new(trace.clone());
    *acl.scan.lock().unwrap() = Some(ScanRefusal::Io(SafeRootError::UnsupportedPlatform));
    let running = spawn(program(&layout, &trace, acl));
    assert_eq!(running.join(), Some(ServiceExit::Specific(3)));
    assert_eq!(acl_calls(&snapshot(&trace)), ["acl:scan"]);
}

/// 2 周目が止まった: 理由を書き、ログを閉じて（flush）から締めた分を緩め直し、Stopped(4)。
/// 緩め直せなければ Stopped(5)。どちらもエンジンは動かさない。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_close_abort_relaxes_back_and_stops_with_4_or_5() {
    let dir = tempfile::tempdir().unwrap();
    let layout = DataLayout::new(dir.path().join("root"));
    prepare_root_db(&layout, None).await;
    for (relax_fails, expected) in [(false, 4), (true, 5)] {
        let trace: Trace = Arc::default();
        let acl = FakeAcl::new(trace.clone());
        *acl.close.lock().unwrap() = Some(AbortReason::Problem(TreeProblem::Denied {
            path: layout.data_dir().join("locked"),
        }));
        acl.relax_fails.store(relax_fails, Ordering::SeqCst);
        let running = spawn(program(&layout, &trace, acl));
        let seen = running.seen.clone();
        assert_eq!(
            running.join(),
            Some(ServiceExit::Specific(expected)),
            "relax_fails={relax_fails}"
        );
        assert!(!has_phase(&seen, ServicePhase::Running));
        let lines = snapshot(&trace);
        assert_eq!(acl_calls(&lines), ["acl:scan", "acl:close", "acl:relax"]);
        let stop = position(&lines, "締めを 1 件で止めました");
        let flush = position(&lines, "flush");
        let relax = position(&lines, "acl:relax");
        assert!(stop < flush && flush < relax, "{lines:#?}");
        assert_eq!(relax, lines.len() - 1, "緩め始めた後に行がある: {lines:#?}");
        assert!(!layout.endpoint_file().exists());
    }
}

/// 締めた後の印で改ざんを見つけた（`DirIsReparse`）: 緩め直して Stopped(5)。エンジンは
/// 動かさない。印の I/O の失敗なら緩め直して 3。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_tampered_marker_relaxes_back_and_stops_with_5() {
    let dir = tempfile::tempdir().unwrap();
    let layout = DataLayout::new(dir.path().join("root"));
    prepare_root_db(&layout, None).await;
    for (error, expected) in [
        (MarkerError::DirIsReparse { tag: 0xA000_0003 }, 5),
        (MarkerError::Busy { os_error: 32 }, 5),
        (MarkerError::Io(SafeRootError::UnsupportedPlatform), 3),
    ] {
        let trace: Trace = Arc::default();
        let acl = FakeAcl::new(trace.clone());
        *acl.marker.lock().unwrap() = Some(("data".to_string(), error.clone()));
        let running = spawn(program(&layout, &trace, acl));
        let seen = running.seen.clone();
        assert_eq!(
            running.join(),
            Some(ServiceExit::Specific(expected)),
            "{error:?}"
        );
        assert!(!has_phase(&seen, ServicePhase::Running), "{error:?}");
        let lines = snapshot(&trace);
        let calls = acl_calls(&lines);
        assert_eq!(calls.first().map(String::as_str), Some("acl:scan"));
        assert_eq!(calls.get(1).map(String::as_str), Some("acl:close"));
        assert!(calls.contains(&"acl:marker:data".to_string()), "{calls:?}");
        assert_eq!(calls.last().map(String::as_str), Some("acl:relax"));
        assert_eq!(position(&lines, "acl:relax"), lines.len() - 1, "{lines:#?}");
        if expected == 5 {
            assert!(
                index_of(&lines, "外から変えられました").is_some(),
                "{lines:#?}"
            );
        }
        assert!(!layout.endpoint_file().exists(), "{error:?}");
    }
}

/// 運用中に定期の確かめ直しが落ちた（`AclChanged`）: エンジンを止め、**締めたまま**
/// Stopped(5)。SCM の STOP を経ないので StopPending は無く、緩めない。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_recheck_failure_during_operation_stops_with_5_and_keeps_the_tree_tight() {
    let dir = tempfile::tempdir().unwrap();
    let layout = DataLayout::new(dir.path().join("root"));
    prepare_root_db(&layout, None).await;
    let trace: Trace = Arc::default();
    let acl = FakeAcl::new(trace.clone());
    let running = spawn(test_program(
        &layout,
        Ok(Some(USER.to_string())),
        &trace,
        Arc::clone(&acl),
        Duration::from_millis(100),
    ));
    // Running と endpoint を確かめてから失敗を入れる（起動前に予約すると、100ms 間隔の
    // `wait_running` が Running を見つける前に改ざんでの停止と endpoint の削除まで進み、
    // 直後の `exists` が偽になることがあった - CI で 1 回）。`recheck_calls` はもう進んで
    // いるので、次の確かめ直しから失敗する。
    wait_running(&running).await;
    assert!(layout.endpoint_file().exists());
    acl.recheck_fails_at.store(1, Ordering::SeqCst);
    let seen = running.seen.clone();
    let exit = wait_for("改ざんでの停止", async || {
        phases(&seen)
            .iter()
            .find(|(phase, _)| *phase == ServicePhase::Stopped)
            .map(|(_, exit)| *exit)
    })
    .await;
    assert_eq!(exit, ServiceExit::Specific(5));
    assert_eq!(running.join(), Some(ServiceExit::Specific(5)));
    assert!(
        !has_phase(&seen, ServicePhase::StopPending),
        "{:?}",
        phases(&seen)
    );
    let lines = snapshot(&trace);
    assert!(
        index_of(&lines, "acl:recheck-failed").is_some(),
        "{lines:#?}"
    );
    assert!(
        index_of(&lines, "権限が外から変えられました").is_some(),
        "{lines:#?}"
    );
    assert!(
        index_of(&lines, "acl:relax").is_none(),
        "改ざんで緩めた: {lines:#?}"
    );
    assert!(!layout.endpoint_file().exists(), "エンジンを止めていない");
    assert!(
        index_of(&lines, "最終 flush を含む停止が完了しました").is_none(),
        "改ざんの停止に STOP の完了の行を出した: {lines:#?}"
    );
}
