//! [`super`]（ランナーの芯）のテスト。偽の登録を渡し、制御を別スレッドから
//! 送って報告の順序を確かめる（全 OS）。

use super::*;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::Duration;
use std::time::Instant;

use crate::shutdown::ServicePhase;

/// 報告を記録する（計画と終わり方）。
type Seen = Arc<Mutex<Vec<(ServiceStatusPlan, ServiceExit)>>>;

/// 偽の登録が返す、報告を記録する関数。
type TestReport = Box<dyn Fn(&ServiceStatusPlan, ServiceExit) + Send + Sync>;

/// 偽の登録: ハンドラをテストへ渡し、報告を記録する関数を返す。
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

/// 本体の振る舞いをクロージャで差し込むテスト用の program。
struct TestProgram<B: Fn(&ServiceContext) -> ServiceExit + Send + Sync + 'static> {
    body: B,
    errors: Arc<Mutex<Vec<String>>>,
    opened: Arc<AtomicBool>,
}

impl<B: Fn(&ServiceContext) -> ServiceExit + Send + Sync + 'static> ServiceProgram
    for TestProgram<B>
{
    fn service_name(&self) -> &str {
        "TestSvc"
    }
    fn label(&self) -> &str {
        "test-svc"
    }
    fn open_log(&mut self) {
        self.opened.store(true, Ordering::SeqCst);
    }
    fn log_error(&self, line: &str) {
        self.errors.lock().unwrap().push(line.to_string());
    }
    fn run(&self, ctx: &ServiceContext) -> ServiceExit {
        (self.body)(ctx)
    }
}

fn program<B>(body: B) -> (TestProgram<B>, Arc<Mutex<Vec<String>>>)
where
    B: Fn(&ServiceContext) -> ServiceExit + Send + Sync + 'static,
{
    let errors = Arc::new(Mutex::new(Vec::new()));
    (
        TestProgram {
            body,
            errors: errors.clone(),
            opened: Arc::new(AtomicBool::new(false)),
        },
        errors,
    )
}

fn phases(seen: &Seen) -> Vec<(ServicePhase, ServiceExit)> {
    seen.lock()
        .unwrap()
        .iter()
        .map(|(plan, exit)| (plan.phase, *exit))
        .collect()
}

/// 本体を別スレッドで動かし、ハンドラを受け取る。
fn spawn_runner<P: ServiceProgram>(
    program: P,
) -> (
    std::thread::JoinHandle<Option<ServiceExit>>,
    ControlHandler,
    Seen,
) {
    let seen: Seen = Arc::new(Mutex::new(Vec::new()));
    let (handler_tx, handler_rx) = mpsc::channel();
    let register = fake_register(handler_tx, seen.clone());
    let runner = std::thread::spawn(move || run_program_with(program, register));
    let handler = handler_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("handler registered");
    (runner, handler, seen)
}

#[test]
fn handler_action_table() {
    // (制御, 1 回目, 2 回目)
    let table = [
        (
            ScmControlKind::Interrogate,
            HandlerAction::Acknowledge,
            HandlerAction::Acknowledge,
        ),
        (
            ScmControlKind::Stop,
            HandlerAction::BeginStop(StopSignal::ServiceStop),
            HandlerAction::Acknowledge,
        ),
        (
            ScmControlKind::Preshutdown,
            HandlerAction::BeginStop(StopSignal::OsPreshutdown),
            HandlerAction::Acknowledge,
        ),
        (
            ScmControlKind::Shutdown,
            HandlerAction::NotImplemented,
            HandlerAction::NotImplemented,
        ),
        (
            ScmControlKind::Other,
            HandlerAction::NotImplemented,
            HandlerAction::NotImplemented,
        ),
    ];
    for (control, first, second) in table {
        let slot = Mutex::new(None);
        assert_eq!(handler_action(control, &slot), first, "{control:?}");
        assert_eq!(handler_action(control, &slot), second, "{control:?}");
    }

    // STOP の後の PRESHUTDOWN は停止をやり直さない（最初の合図のまま）。
    let slot = Mutex::new(None);
    assert_eq!(
        handler_action(ScmControlKind::Stop, &slot),
        HandlerAction::BeginStop(StopSignal::ServiceStop)
    );
    assert_eq!(
        handler_action(ScmControlKind::Preshutdown, &slot),
        HandlerAction::Acknowledge
    );
    assert_eq!(
        slot.lock().unwrap().unwrap().signal,
        StopSignal::ServiceStop
    );
}

#[test]
fn service_exit_maps_to_scm_exit_codes() {
    assert_eq!(ServiceExit::Success.scm_exit_codes(), (0, 0));
    assert_eq!(ServiceExit::Specific(2).scm_exit_codes(), (1066, 2));
    assert_eq!(ServiceExit::Specific(3).scm_exit_codes(), (1066, 3));
}

/// 写した `ServiceExitCode` を `SERVICE_STATUS` へ書いて読み戻すと、
/// [`ServiceExit::scm_exit_codes`] と同じ 2 つの値になる。
#[cfg(windows)]
#[test]
fn service_exit_maps_to_windows_exit_codes() {
    use windows_service::service::ServiceExitCode;
    use windows_sys::Win32::System::Services::SERVICE_STATUS;
    for exit in [
        ServiceExit::Success,
        ServiceExit::Specific(1),
        ServiceExit::Specific(2),
        ServiceExit::Specific(3),
    ] {
        let (win32, specific) = exit.scm_exit_codes();
        let raw = SERVICE_STATUS {
            dwServiceType: 0,
            dwCurrentState: 0,
            dwControlsAccepted: 0,
            dwWin32ExitCode: win32,
            dwServiceSpecificExitCode: specific,
            dwCheckPoint: 0,
            dwWaitHint: 0,
        };
        assert_eq!(
            ServiceExitCode::from(&raw),
            exit.to_service_exit_code(),
            "{exit:?}"
        );
    }
    assert_eq!(
        ServiceExit::Success.to_service_exit_code(),
        ServiceExitCode::NO_ERROR
    );
}

/// (a) Running → STOP → StopPending → 本体の後始末 → Stopped(0)。
/// 同期の待ち（`wait_for_stop`）で。
#[test]
fn running_then_stop_reports_pending_then_stopped_success() {
    let (cleanup_tx, cleanup_rx) = mpsc::channel::<StopNotice>();
    let (prog, errors) = program(move |ctx| {
        assert!(ctx.report_running());
        let notice = ctx.wait_for_stop();
        cleanup_tx.send(notice).unwrap();
        ServiceExit::Success
    });
    let opened = prog.opened.clone();
    let (runner, handler, seen) = spawn_runner(prog);
    assert!(opened.load(Ordering::SeqCst), "登録より前にログを開く");

    // Running の報告を待ってから STOP。
    let deadline = Instant::now() + Duration::from_secs(5);
    while phases(&seen).is_empty() {
        assert!(Instant::now() < deadline, "Running が報告されない");
        std::thread::yield_now();
    }
    assert_eq!(handler(ScmControlKind::Stop), ControlReply::NoError);
    assert_eq!(handler(ScmControlKind::Stop), ControlReply::NoError);
    assert_eq!(
        handler(ScmControlKind::Shutdown),
        ControlReply::NotImplemented
    );
    let notice = cleanup_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("本体が起きる");
    assert_eq!(notice.signal, StopSignal::ServiceStop);
    assert_eq!(runner.join().unwrap(), Some(ServiceExit::Success));

    assert_eq!(
        phases(&seen),
        vec![
            (ServicePhase::Running, ServiceExit::Success),
            (ServicePhase::StopPending, ServiceExit::Success),
            (ServicePhase::Stopped, ServiceExit::Success),
        ]
    );
    assert!(errors.lock().unwrap().is_empty());
}

/// (a') 非同期の待ち（`stopped`）でも PRESHUTDOWN で起きる。
#[test]
fn async_body_wakes_on_preshutdown() {
    let (prog, _errors) = program(|ctx| {
        assert!(ctx.report_running());
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let notice = runtime.block_on(ctx.stopped());
        assert_eq!(notice.signal, StopSignal::OsPreshutdown);
        assert!(notice.os_work_deadline().is_some());
        ctx.drop_after_stopped(runtime);
        ServiceExit::Success
    });
    let (runner, handler, seen) = spawn_runner(prog);
    let deadline = Instant::now() + Duration::from_secs(5);
    while phases(&seen).is_empty() {
        assert!(Instant::now() < deadline, "Running が報告されない");
        std::thread::yield_now();
    }
    assert_eq!(handler(ScmControlKind::Preshutdown), ControlReply::NoError);
    assert_eq!(runner.join().unwrap(), Some(ServiceExit::Success));
    let seen = phases(&seen);
    assert_eq!(seen.last().unwrap().0, ServicePhase::Stopped);
}

/// (b) Running の前に起動に失敗: Stopped(n) を 1 回だけ、Running は
/// 出さない（#330 の `StoppedGate` の置き換え。終了コード 0 で上書き
/// しない）。
#[test]
fn a_start_failure_reports_stopped_once_with_its_code_and_no_running() {
    let (prog, _errors) = program(|_ctx| ServiceExit::Specific(2));
    let (runner, _handler, seen) = spawn_runner(prog);
    assert_eq!(runner.join().unwrap(), Some(ServiceExit::Specific(2)));
    assert_eq!(
        phases(&seen),
        vec![(ServicePhase::Stopped, ServiceExit::Specific(2))]
    );
}

/// (c) 起動中に STOP が届いた: Running を報告せず（`report_running` が
/// false）、StopPending → Stopped。
#[test]
fn a_stop_during_start_suppresses_running() {
    let (go_tx, go_rx) = mpsc::channel::<()>();
    let go_rx = Mutex::new(go_rx);
    let (prog, _errors) = program(move |ctx| {
        // 起動処理の途中で STOP が届くのを待つ。
        go_rx.lock().unwrap().recv().unwrap();
        assert!(ctx.stop_notice().is_some());
        assert!(!ctx.report_running(), "停止の後に Running を出さない");
        let notice = ctx.wait_for_stop();
        assert_eq!(notice.signal, StopSignal::ServiceStop);
        ServiceExit::Success
    });
    let (runner, handler, seen) = spawn_runner(prog);
    assert_eq!(handler(ScmControlKind::Stop), ControlReply::NoError);
    go_tx.send(()).unwrap();
    assert_eq!(runner.join().unwrap(), Some(ServiceExit::Success));
    let seen = phases(&seen);
    assert!(
        !seen
            .iter()
            .any(|(phase, _)| *phase == ServicePhase::Running),
        "{seen:?}"
    );
    assert_eq!(
        seen.last().copied(),
        Some((ServicePhase::Stopped, ServiceExit::Success))
    );
    assert_eq!(
        seen.iter()
            .filter(|(phase, _)| *phase == ServicePhase::Stopped)
            .count(),
        1
    );
}

/// (d) 本体の panic: Stopped(ServiceSpecific(1)) を 1 回だけ、panic を
/// ログに残す（オーナー決定 D2）。
#[test]
fn a_panic_in_the_body_reports_stopped_specific_one_once() {
    let (prog, errors) = program(|ctx| {
        assert!(ctx.report_running());
        panic!("テストの panic");
    });
    let (runner, _handler, seen) = spawn_runner(prog);
    assert_eq!(
        runner.join().unwrap(),
        Some(ServiceExit::Specific(PANIC_EXIT_CODE))
    );
    assert_eq!(
        phases(&seen),
        vec![
            (ServicePhase::Running, ServiceExit::Success),
            (ServicePhase::Stopped, ServiceExit::Specific(1)),
        ]
    );
    let errors = errors.lock().unwrap();
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert!(errors[0].starts_with("test-svc: "), "{}", errors[0]);
    assert!(errors[0].contains("テストの panic"), "{}", errors[0]);
}

/// 登録に失敗したら、その理由をログに書き、本体は動かさず、何も
/// 報告しない（報告の口が無い）。
#[test]
fn a_registration_failure_is_logged_and_runs_nothing() {
    let ran = Arc::new(AtomicBool::new(false));
    let ran_in_body = ran.clone();
    let (prog, errors) = program(move |_ctx| {
        ran_in_body.store(true, Ordering::SeqCst);
        ServiceExit::Success
    });
    let result = run_program_with(prog, |_handler, _log| {
        Err::<fn(&ServiceStatusPlan, ServiceExit), _>("os error 1063".to_string())
    });
    assert_eq!(result, None);
    assert!(!ran.load(Ordering::SeqCst));
    assert_eq!(
        errors.lock().unwrap().as_slice(),
        ["test-svc: サービスコントロールハンドラの登録に失敗しました: os error 1063"]
    );
}

/// 登録から reporter の作成までの間に届いた停止も StopPending にする。
#[test]
fn a_stop_between_registration_and_reporter_enters_stop_pending() {
    let seen: Seen = Arc::new(Mutex::new(Vec::new()));
    let sink = seen.clone();
    let (prog, _errors) = program(|ctx| {
        assert!(!ctx.report_running());
        ctx.wait_for_stop();
        ServiceExit::Success
    });
    let result = run_program_with(prog, move |handler, _log| {
        // 登録の直後（reporter がまだ無い）に STOP が届く。
        assert_eq!(handler(ScmControlKind::Stop), ControlReply::NoError);
        Ok(move |plan: &ServiceStatusPlan, exit: ServiceExit| {
            sink.lock().unwrap().push((*plan, exit))
        })
    });
    assert_eq!(result, Some(ServiceExit::Success));
    assert_eq!(
        phases(&seen),
        vec![
            (ServicePhase::StopPending, ServiceExit::Success),
            (ServicePhase::Stopped, ServiceExit::Success),
        ]
    );
}

/// `drop_after_stopped` に渡したものは Stopped の報告の後で落ちる。
#[test]
fn deferred_values_drop_after_stopped() {
    struct Marker(Seen, Arc<Mutex<Option<usize>>>);
    impl Drop for Marker {
        fn drop(&mut self) {
            *self.1.lock().unwrap() = Some(self.0.lock().unwrap().len());
        }
    }
    let seen: Seen = Arc::new(Mutex::new(Vec::new()));
    let reports_at_drop = Arc::new(Mutex::new(None));
    let marker_seen = seen.clone();
    let marker_slot = reports_at_drop.clone();
    let marker = Mutex::new(Some(Marker(marker_seen, marker_slot)));
    let (prog, _errors) = program(move |ctx| {
        ctx.drop_after_stopped(marker.lock().unwrap().take().unwrap());
        ServiceExit::Specific(3)
    });
    let sink = seen.clone();
    let result = run_program_with(prog, move |_handler, _log| {
        Ok(move |plan: &ServiceStatusPlan, exit: ServiceExit| {
            sink.lock().unwrap().push((*plan, exit))
        })
    });
    assert_eq!(result, Some(ServiceExit::Specific(3)));
    assert_eq!(*reports_at_drop.lock().unwrap(), Some(1), "Stopped の後");
}

/// SCM の外（テストのプロセス）では `NotUnderScm`、2 回目は
/// `AlreadyStarted`。プロセスで 1 回しか呼べないので、このテストだけが
/// `run_service` を呼ぶ。
#[cfg(windows)]
#[test]
fn run_service_outside_scm_then_again_is_already_started() {
    let (first, _) = program(|_ctx| ServiceExit::Success);
    match run_service(first) {
        Err(DispatchError::NotUnderScm(detail)) => assert!(!detail.is_empty()),
        other => panic!("SCM の外では NotUnderScm のはず: {other:?}"),
    }
    let (second, _) = program(|_ctx| ServiceExit::Success);
    assert_eq!(run_service(second), Err(DispatchError::AlreadyStarted));
}

#[test]
fn dispatch_failure_lines_are_shared_across_products() {
    let lines = dispatch_failure_lines(
        "banto-hub-sink",
        &DispatchError::NotUnderScm("os error 1063".to_string()),
    );
    assert_eq!(
        lines[0],
        "banto-hub-sink: service_dispatcher の起動に失敗しました: os error 1063"
    );
    assert_eq!(
        lines[1],
        "banto-hub-sink: 'run-service' は SCM がサービス開始時に呼ぶ内部エントリポイントです。手動では実行できません（`install` → `Start-Service` で起動してください）"
    );
}

#[test]
fn os_work_deadline_is_only_for_preshutdown() {
    let received_at = Instant::now();
    let stop = StopNotice {
        signal: StopSignal::ServiceStop,
        received_at,
    };
    assert_eq!(stop.os_work_deadline(), None);
    let os = StopNotice {
        signal: StopSignal::OsPreshutdown,
        received_at,
    };
    assert_eq!(
        os.os_work_deadline(),
        Some(received_at + Duration::from_secs(27))
    );
}

/// 本体が `hold_until_stopped` で預けたもの（tokio のランタイムの代わりの、
/// drop に時間がかかるダミー）は、本体が panic しても Stopped の報告の**後**で
/// 落ちる。本体の手元の参照はアンワインドで落ちるが、ランナーが持つ参照が
/// 残るため（レビュー Should 2: `block_on` の中の panic で、Stopped の報告が
/// ランタイムの drop を待たないこと）。
#[test]
fn a_held_value_drops_after_stopped_even_when_the_body_panics() {
    struct SlowDrop {
        seen: Seen,
        reports_at_drop: Arc<Mutex<Option<usize>>>,
    }
    impl Drop for SlowDrop {
        fn drop(&mut self) {
            std::thread::sleep(Duration::from_millis(50));
            *self.reports_at_drop.lock().unwrap() = Some(self.seen.lock().unwrap().len());
        }
    }
    let seen: Seen = Arc::new(Mutex::new(Vec::new()));
    let reports_at_drop = Arc::new(Mutex::new(None));
    let dummy = Mutex::new(Some(SlowDrop {
        seen: seen.clone(),
        reports_at_drop: reports_at_drop.clone(),
    }));
    let (prog, _errors) = program(move |ctx| {
        let held = ctx.hold_until_stopped(dummy.lock().unwrap().take().unwrap());
        assert!(ctx.report_running());
        let _in_use = held;
        panic!("block_on の中の panic の代わり");
    });
    let sink = seen.clone();
    let result = run_program_with(prog, move |_handler, _log| {
        Ok(move |plan: &ServiceStatusPlan, exit: ServiceExit| {
            sink.lock().unwrap().push((*plan, exit))
        })
    });
    assert_eq!(result, Some(ServiceExit::Specific(PANIC_EXIT_CODE)));
    assert_eq!(
        phases(&seen),
        vec![
            (ServicePhase::Running, ServiceExit::Success),
            (ServicePhase::Stopped, ServiceExit::Specific(1)),
        ]
    );
    assert_eq!(
        *reports_at_drop.lock().unwrap(),
        Some(2),
        "預けたものは Running と Stopped の報告の後で落ちる"
    );
}

/// 開始の引数（`ServiceMain` の argv）が `ctx.start_args()` に載る。
/// 引数なしの `run_program_with` では空。
#[test]
fn start_args_reach_the_body() {
    let seen_args: Arc<Mutex<Vec<Vec<OsString>>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = seen_args.clone();
    let (prog, _errors) = program(move |ctx| {
        sink.lock().unwrap().push(ctx.start_args().to_vec());
        ServiceExit::Success
    });
    let result = run_program_with_args(
        prog,
        vec![OsString::from("TestSvc"), OsString::from("relax-only")],
        |_handler, _log| Ok(|_plan: &ServiceStatusPlan, _exit: ServiceExit| {}),
    );
    assert_eq!(result, Some(ServiceExit::Success));
    let sink = seen_args.clone();
    let (prog, _errors) = program(move |ctx| {
        sink.lock().unwrap().push(ctx.start_args().to_vec());
        ServiceExit::Success
    });
    run_program_with(prog, |_handler, _log| {
        Ok(|_plan: &ServiceStatusPlan, _exit: ServiceExit| {})
    });
    assert_eq!(
        *seen_args.lock().unwrap(),
        vec![
            vec![OsString::from("TestSvc"), OsString::from("relax-only")],
            Vec::<OsString>::new()
        ]
    );
}

/// relax-only の道: START_PENDING の checkpoint を進め、Running を報告せずに
/// 返すと、報告は StartPending（単調）から Stopped(コード) だけ。
#[test]
fn start_progress_then_return_without_running_goes_start_pending_to_stopped() {
    let (prog, _errors) = program(|ctx| {
        assert!(ctx.report_start_progress(Duration::from_secs(10)));
        assert!(ctx.report_start_progress(Duration::from_secs(10)));
        ServiceExit::Specific(5)
    });
    let (runner, _handler, seen) = spawn_runner(prog);
    assert_eq!(runner.join().unwrap(), Some(ServiceExit::Specific(5)));
    let reported: Vec<(ServicePhase, u32)> = seen
        .lock()
        .unwrap()
        .iter()
        .map(|(plan, _)| (plan.phase, plan.checkpoint))
        .collect();
    assert_eq!(
        reported,
        vec![
            (ServicePhase::StartPending, 1),
            (ServicePhase::StartPending, 2),
            (ServicePhase::Stopped, 0),
        ]
    );
    assert_eq!(
        phases(&seen).last(),
        Some(&(ServicePhase::Stopped, ServiceExit::Specific(5)))
    );
}

/// 起動中に STOP が届いたら `report_start_progress` は出さない（false）。
#[test]
fn start_progress_is_refused_after_a_stop() {
    let (go_tx, go_rx) = mpsc::channel::<()>();
    let go_rx = Mutex::new(go_rx);
    let (prog, _errors) = program(move |ctx| {
        go_rx.lock().unwrap().recv().unwrap();
        assert!(!ctx.report_start_progress(Duration::from_secs(10)));
        ctx.wait_for_stop();
        ServiceExit::Success
    });
    let (runner, handler, _seen) = spawn_runner(prog);
    assert_eq!(handler(ScmControlKind::Stop), ControlReply::NoError);
    go_tx.send(()).unwrap();
    assert_eq!(runner.join().unwrap(), Some(ServiceExit::Success));
}
