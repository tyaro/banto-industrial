//! サービス本体のテスト（#661 661-2 で設定モード ⇔ 運用モードの順序を足した）。
//! **SCM も HKLM も使わない**: 共有ランナーの芯（`run_program_with_args`）に偽の登録を渡し、
//! 制御はテストから送る。profile は一時ディレクトリ、戻す相手の記録は偽の読み書き、ログは
//! 本体が書く行を集める偽物。ACL の付け替えの部品（[`AclOps`]）は偽物（[`FakeAcl`]）で、
//! profile は `Mode::Plain`（`std::fs`）。偽物は呼ばれた順を本体のログの行と**同じ列**に
//! 記録するので、「締め終わる前に Hub を動かさない」「緩め始めた後に書かない」を列の順序で
//! 確かめる（ChronoGazer の #623 PR-3 のテストと同じ方式）。本物の締める・緩めるは
//! `banto-safe-root` の Windows の実物テスト。

use super::*;
use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;

use banto_hub_core::profile_lock::{try_acquire_profile_lock, ProfileOwnerInfo};
use banto_hub_core::profile_paths::resolve_profile_paths;
use banto_hub_core::service_manager::{
    run_program_with_args, ControlHandler, ControlReply, ErrorLog, ScmControlKind, ServicePhase,
    ServiceStatusPlan,
};
use banto_safe_root::acl::{ObservedAce, ObservedDacl};
use banto_safe_root::mode::TreeProblem;

/// 本体が終わるのを待つ上限（退行して止まらなくなっても CI をハングさせない）。
const JOIN_LIMIT: Duration = Duration::from_secs(60);
/// Running を待つ上限。
const WAIT_LIMIT: Duration = Duration::from_secs(40);
const USER: &str = "S-1-5-21-1111111111-2222222222-3333333333-1001";
const SYSTEM: &str = "S-1-5-18";
const ADMINS: &str = "S-1-5-32-544";
const OICI: u8 = 0x03;
const FULL: u32 = 0x001F_01FF;
const MODIFY: u32 = 0x0013_01BF;
const READ: u32 = 0x0012_00A9;

/// 本体の行と偽の部品の記録を 1 つに集める列。
type Trace = Arc<Mutex<Vec<String>>>;

fn push(trace: &Trace, line: impl Into<String>) {
    trace.lock().unwrap().push(line.into());
}

fn snapshot(trace: &Trace) -> Vec<String> {
    trace.lock().unwrap().clone()
}

fn index_of(lines: &[String], needle: &str) -> Option<usize> {
    lines.iter().position(|line| line.contains(needle))
}

fn position(lines: &[String], needle: &str) -> usize {
    index_of(lines, needle).unwrap_or_else(|| panic!("{needle:?} が無い: {lines:#?}"))
}

fn has(lines: &[String], needle: &str) -> bool {
    index_of(lines, needle).is_some()
}

/// `needles` がこの順に現れる。
fn assert_in_order(lines: &[String], needles: &[&str]) {
    let positions: Vec<usize> = needles.iter().map(|n| position(lines, n)).collect();
    assert!(
        positions.windows(2).all(|w| w[0] < w[1]),
        "順序が違う {needles:?} → {positions:?}: {lines:#?}"
    );
}

/// ログを閉じた後は、緩める（`acl:`）しか起きない（約束 2）。
fn assert_silent_after_log_closed(lines: &[String]) {
    let closed = position(lines, "log-closed");
    let after: Vec<&String> = lines[closed + 1..]
        .iter()
        .filter(|line| !line.starts_with("acl:"))
        .collect();
    assert!(after.is_empty(), "ログを閉じた後に書いた: {after:?}");
}

// --- 偽の部品 ----------------------------------------------------------------------

/// [`AclOps`] の偽物。
struct FakeAcl {
    trace: Trace,
    observation: Mutex<RootObservation>,
    scan: Mutex<Option<ScanRefusal>>,
    close: Mutex<Option<AbortReason>>,
    relax_fails: AtomicBool,
    /// 確かめ直しを失敗させる。
    recheck_fails: AtomicBool,
    /// 見張りを立てられなくする。
    watch_fails: AtomicBool,
    /// 偽の見張りに変化を報告させる。
    watch_changed: Arc<AtomicBool>,
}

/// [`SecurityWatcher`] の偽物（`changed` が立つと変化を 1 回報告する）。
struct FakeWatcher {
    changed: Arc<AtomicBool>,
    stopped: Arc<AtomicBool>,
}

impl SecurityWatcher for FakeWatcher {
    fn wait(&mut self, timeout: Duration) -> Result<WatchEvent, String> {
        let deadline = Instant::now() + timeout;
        loop {
            if self.stopped.load(Ordering::SeqCst) {
                return Ok(WatchEvent::Stopped);
            }
            if self.changed.load(Ordering::SeqCst) {
                return Ok(WatchEvent::Changed(vec!["config".to_string()]));
            }
            if Instant::now() >= deadline {
                return Ok(WatchEvent::Timeout);
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn stopper(&self) -> Box<dyn Fn() + Send + Sync> {
        let stopped = self.stopped.clone();
        Box::new(move || stopped.store(true, Ordering::SeqCst))
    }
}

impl FakeAcl {
    fn new(trace: &Trace, observation: RootObservation) -> Arc<Self> {
        Arc::new(Self {
            trace: trace.clone(),
            observation: Mutex::new(observation),
            scan: Mutex::new(None),
            close: Mutex::new(None),
            relax_fails: AtomicBool::new(false),
            recheck_fails: AtomicBool::new(false),
            watch_fails: AtomicBool::new(false),
            watch_changed: Arc::new(AtomicBool::new(false)),
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

    fn observe(&self, _profile_dir: &Path) -> Result<RootObservation, String> {
        Ok(self.observation.lock().unwrap().clone())
    }

    fn account_kind(&self, _sid: &InstallUserSid) -> AccountKind {
        AccountKind::User
    }

    fn scan_tree(
        &self,
        _root: &SafeRoot,
        _stop: &dyn Fn() -> TreeStop,
        progress: &mut dyn FnMut(&Progress),
    ) -> Result<ScanReport, ScanRefusal> {
        push(&self.trace, "acl:scan");
        tick(progress);
        match self.scan.lock().unwrap().take() {
            Some(refusal) => Err(refusal),
            None => Ok(ScanReport { examined: 3 }),
        }
    }

    fn close_tree(
        &self,
        _root: &SafeRoot,
        mode: &ModeAccess,
        _stop: &dyn Fn() -> TreeStop,
        progress: &mut dyn FnMut(&Progress),
    ) -> Result<Progress, CloseAbort> {
        let users: Vec<&str> = mode.users().iter().map(InstallUserSid::as_str).collect();
        push(&self.trace, format!("acl:close {}", users.join(",")));
        tick(progress);
        match self.close.lock().unwrap().take() {
            Some(reason) => Err(CloseAbort { written: 2, reason }),
            None => Ok(Progress {
                examined: 3,
                written: 3,
            }),
        }
    }

    fn relax_tree(
        &self,
        _root: &SafeRoot,
        mode: &ModeAccess,
        progress: &mut dyn FnMut(&Progress),
    ) -> Result<RelaxReport, SafeRootError> {
        let users: Vec<&str> = mode.users().iter().map(InstallUserSid::as_str).collect();
        push(&self.trace, format!("acl:relax {}", users.join(",")));
        tick(progress);
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
        push(&self.trace, format!("acl:marker {name}"));
        Ok(MarkerOutcome::Created)
    }

    fn recheck(&self, _dir: &ManagedDir, _expected: &DirAccess) -> Result<(), RecheckFailure> {
        if self.recheck_fails.load(Ordering::SeqCst) {
            push(&self.trace, "acl:recheck-failed");
            return Err(RecheckFailure::NotADirectory);
        }
        Ok(())
    }

    fn watch(&self, _root: &SafeRoot) -> Result<Option<Box<dyn SecurityWatcher>>, String> {
        if self.watch_fails.load(Ordering::SeqCst) {
            push(&self.trace, "acl:watch-failed");
            return Err("偽の見張りの失敗".to_string());
        }
        push(&self.trace, "acl:watch");
        Ok(Some(Box::new(FakeWatcher {
            changed: self.watch_changed.clone(),
            stopped: Arc::new(AtomicBool::new(false)),
        })))
    }
}

/// 行を集めるサービスログ。
struct FakeLog(Trace);

impl ServiceLog for FakeLog {
    fn open(&self, _logs: &ManagedDir) -> Result<(), String> {
        push(&self.0, "log-opened");
        Ok(())
    }

    fn line(&self, line: &str) {
        push(&self.0, line);
    }

    fn error(&self, line: &str) {
        push(&self.0, format!("err: {line}"));
    }

    fn sync(&self) {}

    fn close(&self) {
        push(&self.0, "log-closed");
    }
}

/// 一時の Hub のルート（drop で消す）。
struct TempRoot(PathBuf);

impl TempRoot {
    fn new(tag: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "win-service-661-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        Self(path)
    }
}

impl Drop for TempRoot {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn ace(sid: &str, mask: u32) -> ObservedAce {
    ObservedAce {
        ace_type: 0,
        flags: OICI,
        mask,
        sid: sid.to_string(),
    }
}

fn observed(owner: &str, users: &[(&str, u32)]) -> RootObservation {
    let mut aces = vec![ace(SYSTEM, FULL), ace(ADMINS, FULL)];
    aces.extend(users.iter().map(|(sid, mask)| ace(sid, *mask)));
    RootObservation::Present(ObservedDacl {
        protected: true,
        owner: owner.to_string(),
        aces: Some(aces),
    })
}

/// 設定モードの形（利用者 USER が「変更」で所有者）。
fn loose() -> RootObservation {
    observed(USER, &[(USER, MODIFY)])
}

/// 運用モードの形（利用者は「読み取りと実行」、所有者は Administrators）。
fn tight() -> RootObservation {
    observed(ADMINS, &[(USER, READ)])
}

/// テストの組。
struct Fixture {
    _root: TempRoot,
    trace: Trace,
    acl: Arc<FakeAcl>,
    stored: Arc<Mutex<Option<Vec<String>>>>,
    paths: ProfilePaths,
}

impl Fixture {
    fn new(tag: &str, observation: RootObservation, stored: Option<&[&str]>) -> Self {
        let root = TempRoot::new(tag);
        let profile_id = format!("t661-{tag}-{}", std::process::id());
        let paths = resolve_profile_paths(&root.0, &profile_id).expect("valid profile id");
        std::fs::create_dir_all(&paths.profile_dir).unwrap();
        let trace: Trace = Arc::default();
        let acl = FakeAcl::new(&trace, observation);
        Self {
            _root: root,
            acl,
            stored: Arc::new(Mutex::new(
                stored.map(|items| items.iter().map(|s| s.to_string()).collect()),
            )),
            trace,
            paths,
        }
    }

    fn program(&self) -> HubProgram {
        let paths = self.paths.clone();
        let read = self.stored.clone();
        let write = self.stored.clone();
        let write_trace = self.trace.clone();
        HubProgram::new(ProgramParts {
            config: Box::new(move || {
                let config = HubConfig {
                    drivers: crate::hub_drivers(),
                    db_path: paths.db_path.to_string_lossy().into_owned(),
                    allow_setup: false,
                    port_override: Some(0),
                    bind_override: Some("127.0.0.1".to_string()),
                    data_dir_override: Some(paths.data_dir.clone()),
                    profile_id: paths.profile_id.clone(),
                    host_kind: HubHostKind::Service,
                    skip_profile_lock: false,
                };
                (paths.clone(), config)
            }),
            read_owners: Box::new(move |_id| Ok(read.lock().unwrap().clone())),
            write_owners: Box::new(move |_id, owners| {
                let items: Vec<String> = owners.iter().map(|s| s.as_str().to_string()).collect();
                push(&write_trace, format!("owners:write {}", items.join(",")));
                *write.lock().unwrap() = Some(items);
                Ok(())
            }),
            known_folders: Box::new(|| Ok(KnownFolders::default())),
            redirection_guard: Box::new(|| RedirectionGuard::NotWindows),
            acl: self.acl.clone(),
            log: Arc::new(FakeLog(self.trace.clone())),
        })
    }

    fn lines(&self) -> Vec<String> {
        snapshot(&self.trace)
    }
}

// --- 偽の登録 ------------------------------------------------------------------------

type Seen = Arc<Mutex<Vec<(ServiceStatusPlan, ServiceExit)>>>;
type TestReport = Box<dyn Fn(&ServiceStatusPlan, ServiceExit) + Send + Sync>;

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

fn has_phase(seen: &Seen, wanted: ServicePhase) -> bool {
    seen.lock()
        .unwrap()
        .iter()
        .any(|(plan, _)| plan.phase == wanted)
}

/// 動いているサービス（別スレッドの `run_program_with_args`）。
struct Running {
    handler: ControlHandler,
    seen: Seen,
    thread: Option<std::thread::JoinHandle<()>>,
    done: mpsc::Receiver<Option<ServiceExit>>,
}

impl Running {
    fn send(&self, control: ScmControlKind) {
        assert_eq!((self.handler)(control), ControlReply::NoError);
    }

    fn wait_running(&self) {
        let deadline = Instant::now() + WAIT_LIMIT;
        while !has_phase(&self.seen, ServicePhase::Running) {
            assert!(Instant::now() < deadline, "Running が報告されない");
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    fn join(mut self) -> Option<ServiceExit> {
        match self.done.recv_timeout(JOIN_LIMIT) {
            Ok(exit) => {
                let _ = self.thread.take().unwrap().join();
                exit
            }
            Err(_) => {
                let _ = (self.handler)(ScmControlKind::Stop);
                let _ = self.done.recv_timeout(JOIN_LIMIT);
                let _ = self.thread.take().unwrap().join();
                panic!("本体が {JOIN_LIMIT:?} 以内に終わらなかった");
            }
        }
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        if let Some(thread) = self.thread.take() {
            let _ = (self.handler)(ScmControlKind::Stop);
            let _ = thread.join();
        }
    }
}

fn spawn_with_args(program: HubProgram, args: &[&str]) -> Running {
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

fn spawn(program: HubProgram) -> Running {
    spawn_with_args(program, &[])
}

// --- 運用モードへ・設定モードへ -----------------------------------------------------

/// 設定モードの形から締めて動き、STOP で「Hub を止める → 設定 DB を閉じる → ランタイムを
/// 終える → 最後の行 → ログを閉じる → 緩める」の順になる。相手は締める直前に記録する。
#[test]
fn stop_relaxes_after_the_hub_the_db_and_the_runtime_have_finished() {
    let fx = Fixture::new("stop", loose(), None);
    let running = spawn(fx.program());
    running.wait_running();
    running.send(ScmControlKind::Stop);
    assert_eq!(running.join(), Some(ServiceExit::Success));

    let lines = fx.lines();
    assert_in_order(
        &lines,
        &[
            "log-opened",
            "運用モードへの切り替えを始めます",
            "相手を 1 人決めました",
            "acl:scan",
            &format!("owners:write {USER}"),
            &format!("acl:close {USER}"),
            "acl:marker config",
            "acl:marker audit-spool",
            "Windows サービスとして起動しました",
            "運用モードへの切り替えを終えました",
            "停止要求を受けました",
            DB_CLOSED_LINE,
            RUNTIME_FINISHED_LINE,
            "緩め始めます",
            "log-closed",
            &format!("acl:relax {USER}"),
        ],
    );
    assert_silent_after_log_closed(&lines);
    assert_eq!(
        *fx.stored.lock().unwrap(),
        Some(vec![USER.to_string()]),
        "締める直前に記録した"
    );
}

/// 1 周目が断ったら何も変えずに 4（記録も書かない・締めない・緩めない）。
#[test]
fn a_scan_refusal_changes_nothing_and_stops_with_4() {
    let fx = Fixture::new("scan", loose(), None);
    *fx.acl.scan.lock().unwrap() = Some(ScanRefusal::Problems(vec![TreeProblem::MovedOut {
        path: PathBuf::from("a"),
        now: PathBuf::from("b"),
    }]));
    let running = spawn(fx.program());
    let seen = running.seen.clone();
    assert_eq!(running.join(), Some(ServiceExit::Specific(4)));
    assert!(!has_phase(&seen, ServicePhase::Running));
    let lines = fx.lines();
    assert!(has(&lines, "締められない物が 1 件"), "{lines:#?}");
    for absent in ["owners:write", "acl:close", "acl:relax"] {
        assert!(!has(&lines, absent), "{absent}: {lines:#?}");
    }
    assert_eq!(*fx.stored.lock().unwrap(), None);
}

/// 2 周目が途中で止まったら（木の外へ移された物など）、緩め直して 4。Hub は動かさない。
#[test]
fn a_close_abort_relaxes_back_and_stops_with_4() {
    let fx = Fixture::new("close", loose(), None);
    *fx.acl.close.lock().unwrap() = Some(AbortReason::Problem(TreeProblem::MovedOut {
        path: PathBuf::from("a"),
        now: PathBuf::from("b"),
    }));
    let running = spawn(fx.program());
    let seen = running.seen.clone();
    assert_eq!(running.join(), Some(ServiceExit::Specific(4)));
    assert!(!has_phase(&seen, ServicePhase::Running));
    let lines = fx.lines();
    assert_in_order(
        &lines,
        &[
            "acl:close",
            "締めた分を緩め直します",
            "log-closed",
            "acl:relax",
        ],
    );
    assert_silent_after_log_closed(&lines);
    assert!(!has(&lines, "acl:marker"), "{lines:#?}");
}

/// 緩め直せなければ 5。
#[test]
fn a_failed_relax_back_stops_with_5() {
    let fx = Fixture::new("relax-back", loose(), None);
    *fx.acl.close.lock().unwrap() = Some(AbortReason::Io(SafeRootError::UnsupportedPlatform));
    fx.acl.relax_fails.store(true, Ordering::SeqCst);
    let running = spawn(fx.program());
    assert_eq!(running.join(), Some(ServiceExit::Specific(5)));
    assert!(has(&fx.lines(), "acl:relax"));
}

/// STOP で緩められなければ 5。
#[test]
fn a_failed_relax_on_stop_stops_with_5() {
    let fx = Fixture::new("relax-stop", loose(), None);
    fx.acl.relax_fails.store(true, Ordering::SeqCst);
    let running = spawn(fx.program());
    running.wait_running();
    running.send(ScmControlKind::Stop);
    assert_eq!(running.join(), Some(ServiceExit::Specific(5)));
}

/// PRESHUTDOWN（OS の終了）では緩めない（運用モードは再起動をまたいで続く）。
#[test]
fn preshutdown_keeps_the_profile_tight() {
    let fx = Fixture::new("preshutdown", loose(), None);
    let running = spawn(fx.program());
    running.wait_running();
    running.send(ScmControlKind::Preshutdown);
    assert_eq!(running.join(), Some(ServiceExit::Success));
    let lines = fx.lines();
    assert!(has(&lines, "OS のシャットダウンを受けました"), "{lines:#?}");
    assert!(has(&lines, "ACL を緩めずに終わります"), "{lines:#?}");
    assert!(!has(&lines, "acl:relax"), "{lines:#?}");
}

/// 設定 DB を閉じ切れなければ、緩めずに 5（ランタイムは終える）。
#[test]
fn an_unclosed_db_keeps_the_profile_tight_with_5() {
    let fx = Fixture::new("db", loose(), None);
    let program = fx.program();
    program.fake_db_not_closed.store(true, Ordering::SeqCst);
    let running = spawn(program);
    running.wait_running();
    running.send(ScmControlKind::Stop);
    assert_eq!(running.join(), Some(ServiceExit::Specific(5)));
    let lines = fx.lines();
    assert!(has(&lines, "設定 DB を閉じ切れなかった"), "{lines:#?}");
    assert!(has(&lines, RUNTIME_FINISHED_LINE), "{lines:#?}");
    assert!(!has(&lines, "acl:relax"), "{lines:#?}");
    assert!(!has(&lines, "緩め始めます"), "{lines:#?}");
}

// --- 運用中の見張り（#697 レビュー: 締める前からの `WRITE_DAC` のハンドルでの書き戻し） ---

/// 運用中の改ざんで止まったときの共通の確かめ: 5・緩めない・Hub と設定 DB は止めた。
fn assert_left_tight_after_tamper(fx: &Fixture, running: Running, cause: &str) {
    assert_eq!(running.join(), Some(ServiceExit::Specific(5)));
    let lines = fx.lines();
    assert_in_order(
        &lines,
        &[
            "acl:watch",
            "Windows サービスとして起動しました",
            "運用中に profile の権限が外から変えられました",
            DB_CLOSED_LINE,
            RUNTIME_FINISHED_LINE,
            "log-closed",
        ],
    );
    assert!(has(&lines, cause), "{lines:#?}");
    assert!(!has(&lines, "acl:relax"), "{lines:#?}");
    assert!(!has(&lines, "緩め始めます"), "{lines:#?}");
    assert_silent_after_log_closed(&lines);
}

/// 見張りが変化を報告したら、Hub を止め・設定 DB を閉じ・緩めずに 5。
#[test]
fn a_watched_acl_change_stops_with_5_without_relaxing() {
    let fx = Fixture::new("watch-change", loose(), None);
    let running = spawn(fx.program());
    running.wait_running();
    fx.acl.watch_changed.store(true, Ordering::SeqCst);
    assert_left_tight_after_tamper(&fx, running, "見張りが変化を見つけました（config）");
}

/// 定期の確かめ直しが失敗したら、緩めずに 5。
#[test]
fn a_failed_recheck_stops_with_5_without_relaxing() {
    let fx = Fixture::new("recheck", loose(), None);
    let mut program = fx.program();
    program.recheck_interval = Duration::from_millis(50);
    let running = spawn(program);
    running.wait_running();
    fx.acl.recheck_fails.store(true, Ordering::SeqCst);
    assert_left_tight_after_tamper(&fx, running, "acl:recheck-failed");
    assert!(has(&fx.lines(), "定期の確かめ直しで「profile」が"));
}

/// 見張りを立てられなければ、Hub を動かさずに緩め直して 3（ChronoGazer と同じ扱い）。
#[test]
fn a_watch_that_cannot_start_relaxes_back_before_the_hub_runs() {
    let fx = Fixture::new("watch-fail", loose(), None);
    fx.acl.watch_fails.store(true, Ordering::SeqCst);
    let running = spawn(fx.program());
    assert_eq!(
        running.join(),
        Some(ServiceExit::Specific(HUB_EXIT_START_FAILED))
    );
    let lines = fx.lines();
    assert_in_order(
        &lines,
        &[
            &format!("acl:close {USER}"),
            "acl:marker audit-spool",
            "acl:watch-failed",
            "セキュリティの見張りを始められませんでした",
            "log-closed",
            &format!("acl:relax {USER}"),
        ],
    );
    assert!(
        !has(&lines, "Windows サービスとして起動しました"),
        "{lines:#?}"
    );
}

// --- 戻す相手 ------------------------------------------------------------------------

/// 「変更」以上の利用者が 0 人なら、何も変えずに 4（記録を書かない・確かめも締めもしない）。
#[test]
fn zero_owners_change_nothing_and_stop_with_4() {
    let fx = Fixture::new("no-owner", observed(ADMINS, &[]), None);
    let running = spawn(fx.program());
    assert_eq!(running.join(), Some(ServiceExit::Specific(4)));
    let lines = fx.lines();
    assert!(has(&lines, "grant-profile-acl"), "{lines:#?}");
    for absent in ["owners:write", "acl:scan", "acl:close", "acl:relax"] {
        assert!(!has(&lines, absent), "{absent}: {lines:#?}");
    }
}

/// 締めた形からの再開（PRESHUTDOWN の後・締めかけで落ちた後）は記録を使い、DACL から
/// 採り直さず、記録を上書きしない。
#[test]
fn a_tight_profile_resumes_with_the_record_without_rewriting_it() {
    let other = "S-1-5-21-1111111111-2222222222-3333333333-1002";
    let fx = Fixture::new("resume", tight(), Some(&[other]));
    let running = spawn(fx.program());
    running.wait_running();
    running.send(ScmControlKind::Stop);
    assert_eq!(running.join(), Some(ServiceExit::Success));
    let lines = fx.lines();
    assert!(!has(&lines, "owners:write"), "{lines:#?}");
    assert_in_order(
        &lines,
        &[
            "記録した相手 1 人を使います",
            &format!("acl:close {other}"),
            &format!("acl:relax {other}"),
        ],
    );
    assert_eq!(*fx.stored.lock().unwrap(), Some(vec![other.to_string()]));
}

/// 締まった形 + 有効な記録から再開して 1 周目が断ったら、何も変えずに 5（#699 レビュー
/// P2）。4（緩んだまま）と返すと、呼び出し側が締まったままの profile で Desktop を許す。
#[test]
fn a_scan_refusal_while_resuming_tight_stops_with_5_without_changes() {
    let fx = Fixture::new("resume-scan", tight(), Some(&[USER]));
    *fx.acl.scan.lock().unwrap() = Some(ScanRefusal::Problems(vec![TreeProblem::MovedOut {
        path: PathBuf::from("a"),
        now: PathBuf::from("b"),
    }]));
    let running = spawn(fx.program());
    let seen = running.seen.clone();
    assert_eq!(running.join(), Some(ServiceExit::Specific(5)));
    assert!(!has_phase(&seen, ServicePhase::Running));
    let lines = fx.lines();
    assert!(has(&lines, "締められない物が 1 件"), "{lines:#?}");
    assert!(has(&lines, "締まったまま"), "{lines:#?}");
    assert!(!has(&lines, "設定モードのまま"), "{lines:#?}");
    for absent in ["owners:write", "acl:close", "acl:relax"] {
        assert!(!has(&lines, absent), "{absent}: {lines:#?}");
    }
    assert_eq!(*fx.stored.lock().unwrap(), Some(vec![USER.to_string()]));
}

/// 締まっているのに記録が無いなら、何も変えずに 5（修復を案内する）。
#[test]
fn a_tight_profile_without_a_record_stops_with_5() {
    let fx = Fixture::new("no-record", tight(), None);
    let running = spawn(fx.program());
    assert_eq!(running.join(), Some(ServiceExit::Specific(5)));
    let lines = fx.lines();
    assert!(has(&lines, "grant-profile-acl"), "{lines:#?}");
    for absent in ["owners:write", "acl:scan", "acl:close", "acl:relax"] {
        assert!(!has(&lines, absent), "{absent}: {lines:#?}");
    }
}

// --- relax-only・開始の引数 --------------------------------------------------------

/// `relax-only` は記録の相手で緩めるだけで、Running を報告しない。
#[test]
fn relax_only_relaxes_with_the_record_without_running() {
    let fx = Fixture::new("relax-only", tight(), Some(&[USER]));
    let running = spawn_with_args(fx.program(), &["relax-only"]);
    let seen = running.seen.clone();
    assert_eq!(running.join(), Some(ServiceExit::Success));
    assert!(!has_phase(&seen, ServicePhase::Running));
    let lines = fx.lines();
    assert_in_order(
        &lines,
        &[
            "relax-only で開始しました",
            "log-closed",
            &format!("acl:relax {USER}"),
        ],
    );
    for absent in ["owners:write", "acl:scan", "acl:close"] {
        assert!(!has(&lines, absent), "{absent}: {lines:#?}");
    }
}

/// `relax-only` で記録が無く締まっているなら、緩めずに 5。
#[test]
fn relax_only_without_a_record_keeps_tight_with_5() {
    let fx = Fixture::new("relax-only-none", tight(), None);
    let running = spawn_with_args(fx.program(), &["relax-only"]);
    assert_eq!(running.join(), Some(ServiceExit::Specific(5)));
    assert!(!has(&fx.lines(), "acl:relax"));
}

/// 知らない開始の引数は何も変えずに 3。
#[test]
fn an_unknown_start_arg_changes_nothing_and_stops_with_3() {
    let fx = Fixture::new("unknown-arg", loose(), None);
    let running = spawn_with_args(fx.program(), &["relax_only"]);
    assert_eq!(running.join(), Some(ServiceExit::Specific(3)));
    let lines = fx.lines();
    assert!(
        !lines.iter().any(|line| line.starts_with("acl:")),
        "{lines:#?}"
    );
}

/// 別のプロセスが profile を握っていれば、何も変えずに 2。
#[test]
fn a_held_profile_lock_changes_nothing_and_stops_with_2() {
    let fx = Fixture::new("held", loose(), None);
    let _held = try_acquire_profile_lock(&fx.paths, HubHostKind::Shell).expect("先に握る");
    let running = spawn(fx.program());
    assert_eq!(running.join(), Some(ServiceExit::Specific(2)));
    let lines = fx.lines();
    assert!(
        !lines.iter().any(|line| line.starts_with("acl:")),
        "{lines:#?}"
    );
    assert!(!has(&lines, "owners:write"), "{lines:#?}");
}

// --- 以前からのテスト ----------------------------------------------------------------

#[test]
fn describe_start_failure_reports_profile_lock_with_owner_diagnostics() {
    let err = HubStartError::ProfileLock(ProfileLockError::AlreadyHeld {
        profile_id: "default".to_string(),
        owner: Some(ProfileOwnerInfo {
            pid: 4242,
            host_kind: "shell".to_string(),
            acquired_at_unix_ms: 0,
        }),
    });

    let (exit_code, message) = describe_start_failure(&err);

    assert_eq!(exit_code, HUB_EXIT_PROFILE_LOCK_HELD);
    assert!(message.contains("default"), "message: {message}");
    assert!(message.contains("4242"), "message: {message}");
    assert!(message.contains("shell"), "message: {message}");
}

#[test]
fn describe_start_failure_reports_profile_lock_with_unknown_owner() {
    let err = HubStartError::ProfileLock(ProfileLockError::AlreadyHeld {
        profile_id: "default".to_string(),
        owner: None,
    });

    let (exit_code, message) = describe_start_failure(&err);

    assert_eq!(exit_code, HUB_EXIT_PROFILE_LOCK_HELD);
    assert!(message.contains("unknown"), "message: {message}");
}

#[test]
fn describe_start_failure_reports_generic_code_for_other_start_errors() {
    let err = HubStartError::UnsafeCommissioningBind("test diagnostic message".to_string());

    let (exit_code, message) = describe_start_failure(&err);

    assert_eq!(exit_code, HUB_EXIT_START_FAILED);
    assert!(
        message.contains("test diagnostic message"),
        "message: {message}"
    );
}

/// 通常の STOP と OS の PRESHUTDOWN をログで区別する（実機確認の目印）。
#[test]
fn hub_stop_lines_distinguish_scm_stop_from_os_preshutdown() {
    let stop = hub_stop_reason_line(StopSignal::ServiceStop);
    assert!(stop.contains("SCM の STOP"), "{stop}");
    assert!(stop.contains("最終 flush"), "{stop}");
    let os = hub_stop_reason_line(StopSignal::OsPreshutdown);
    assert!(os.contains("OS のシャットダウン"), "{os}");
    assert!(os.contains("PRESHUTDOWN"), "{os}");

    assert!(hub_stop_done_line(StopSignal::ServiceStop).contains("SCM の STOP"));
    assert!(hub_stop_done_line(StopSignal::OsPreshutdown).contains("PRESHUTDOWN"));
    let overrun = hub_stop_overrun_line();
    assert!(overrun.contains("warn"), "{overrun}");
    assert!(overrun.contains("予算"), "{overrun}");
}

/// 2026-09-07 I4 実機検証で見つかったバグの回帰テスト: シェルが既に profile ロックを
/// 保持している状態で `HubRuntime::start` を叩くと、固まらず即座（5 秒未満）に
/// `HubStartError::ProfileLock(AlreadyHeld)` で返る。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hub_runtime_start_fails_fast_when_profile_lock_already_held() {
    let root = TempRoot::new("lock");
    let profile_id = "locked";

    let paths = resolve_profile_paths(&root.0, profile_id).expect("valid profile id");
    let _existing_lock =
        try_acquire_profile_lock(&paths, HubHostKind::Shell).expect("first acquire should succeed");

    let config = HubConfig {
        drivers: crate::hub_drivers(),
        db_path: root
            .0
            .join("registry.sqlite3")
            .to_string_lossy()
            .into_owned(),
        allow_setup: false,
        port_override: Some(0),
        bind_override: Some("127.0.0.1".to_string()),
        data_dir_override: Some(root.0.join("data")),
        profile_id: profile_id.to_string(),
        host_kind: HubHostKind::Service,
        skip_profile_lock: false,
    };

    // `HubRuntime::start`の profile root 解決は`BANTO_HUB_ROOT`を読む。
    std::env::set_var("BANTO_HUB_ROOT", root.0.to_string_lossy().as_ref());
    let outcome = tokio::time::timeout(Duration::from_secs(5), HubRuntime::start(config)).await;
    std::env::remove_var("BANTO_HUB_ROOT");

    let result = outcome
        .expect("HubRuntime::start must return within 5s instead of hanging like the observed bug");
    match result {
        Err(err @ HubStartError::ProfileLock(ProfileLockError::AlreadyHeld { .. })) => {
            let (exit_code, _message) = describe_start_failure(&err);
            assert_eq!(exit_code, HUB_EXIT_PROFILE_LOCK_HELD);
        }
        Err(other) => panic!("expected ProfileLock(AlreadyHeld), got: {other}"),
        Ok(hub) => {
            hub.shutdown().await;
            panic!("expected HubRuntime::start to fail while the profile lock is held");
        }
    }
}
